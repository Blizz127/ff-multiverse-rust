//! Owned-data regression for opaque PC palette entries whose raw colour is zero. The
//! archive is opened read-only; only pixel assertions and counts are retained, never assets.
//!
//! Independent colour rule: Makou Reactor commit 574e4ba5c2bdef6fd604e75790ecb41558e08f9b,
//! `src/core/field/Palette.cpp`, lines 34-41 and 99-110: raw zero uses entry 0's RGB;
//! transparency still depends on the texture index and the palette's transparency flag.
//! https://github.com/myst6re/makoureactor/blob/574e4ba5c2bdef6fd604e75790ecb41558e08f9b/src/core/field/Palette.cpp

use fmv_ff7::background::{decode_background, decode_background_tiles_with_depth, decode_background_with_palette, BackgroundOptions};
use fmv_ff7::field::FieldFile;
use fmv_ff7::lgp::LgpArchive;
use std::path::PathBuf;

#[test]
#[ignore = "needs the owner's original FF7 field archive (FMV_FF7_DATA)"]
fn station_raw_zero_colours_preserve_wall_pixels_without_filling_transparent_holes() {
    let Some(data) = std::env::var_os("FMV_FF7_DATA") else {
        eprintln!("FMV_FF7_DATA is needed; skipping");
        return;
    };
    let mut archive = LgpArchive::open_path(PathBuf::from(data).join("field/flevel.lgp")).expect("owned field archive");
    let packed = archive.read("md1stin").expect("station field");
    let field = FieldFile::parse(fmv_ff7::lzss::decompress(&packed).expect("field compression")).expect("field");
    let background = field.background().unwrap();
    let palette = field.palette().unwrap();
    let image = decode_background(&field).unwrap();
    let tiles = decode_background_tiles_with_depth(&field).unwrap();

    // Field-screen position, layer/tile, texture-page pixel, palette/index and expected
    // colour. Both formerly black specks are opaque pixels, including the foreground wall.
    for (x, y, layer, tile_index, slot, u, v, page, index, entry_zero, expected) in [
        (-73, -76, 2, 150, 3, 23, 180, 11, 208, 0x10c7, [57, 49, 33, 255]),
        (-113, -64, 1, 360, 1, 143, 96, 2, 6, 0x0464, [33, 24, 8, 255]),
    ] {
        let raw_tile = background.tiles.iter().find(|tile| tile.layer == layer && tile.index == tile_index).unwrap();
        assert_eq!(usize::from(raw_tile.palette_id), page);
        let (source_slot, sx, sy) = raw_tile.effective_source();
        let tx = u32::try_from(x - i32::from(raw_tile.dst_x)).unwrap();
        let ty = u32::try_from(y - i32::from(raw_tile.dst_y)).unwrap();
        assert_eq!((source_slot, u32::from(sx) + tx, u32::from(sy) + ty), (slot, u, v));
        let texture = background.texture(source_slot).unwrap();
        assert_eq!(texture.depth, 1);
        assert_eq!(usize::from(texture.pixels[(v * 256 + u) as usize]), index);
        assert_ne!(index, 0, "the speck is not a transparent texture index");
        assert_eq!(palette.raw(page, index), Some(0));
        assert_eq!(palette.raw(page, 0), Some(entry_zero));
        assert!(background.palette_transparency[page], "the index-zero transparency key remains enabled");
        assert_eq!(image.pixel((x + image.origin_x) as u32, (y + image.origin_y) as u32), Some(expected));
        let tile = tiles.iter().find(|tile| tile.layer == layer && tile.index == tile_index).unwrap();
        assert_eq!(tile.pixel(tx, ty), Some(expected), "the depth renderer receives the same opaque colour");
    }

    // A genuine index-zero hole nearby remains transparent even though entry 0 is nonblack.
    let (x, y) = (-125, -105);
    let raw_tile = background.tiles.iter().find(|tile| tile.layer == 1 && tile.index == 334).unwrap();
    assert_eq!(raw_tile.palette_id, 2);
    assert!(background.palette_transparency[2]);
    assert_eq!(palette.raw(2, 0), Some(0x0464));
    let (slot, sx, sy) = raw_tile.effective_source();
    let tx = u32::try_from(x - i32::from(raw_tile.dst_x)).unwrap();
    let ty = u32::try_from(y - i32::from(raw_tile.dst_y)).unwrap();
    assert_eq!((slot, u32::from(sx) + tx, u32::from(sy) + ty), (1, 227, 71));
    let texture = background.texture(slot).unwrap();
    assert_eq!(texture.depth, 1);
    assert_eq!(texture.pixels[71 * 256 + 227], 0);
    let tile = tiles.iter().find(|tile| tile.layer == 1 && tile.index == 334).unwrap();
    assert_eq!(tile.pixel(tx, ty).unwrap()[3], 0);
    assert_eq!(image.pixel((x + image.origin_x) as u32, (y + image.origin_y) as u32).unwrap()[3], 0);

    // Recreate the old black RGB for raw-zero nonzero indices on a palette clone only.
    // 0x8000 is opaque black, distinct from the exact-zero substitution, and preserves
    // the original index and therefore its alpha. No archive bytes or assets are written.
    let mut old_palette = palette.clone();
    for page in 0..palette.page_count() {
        for index in 1..usize::from(palette.colors_per_page) {
            if palette.raw(page, index) == Some(0) {
                assert_eq!(old_palette.set_raw(page, index, 0x8000), Some(true));
            }
        }
    }
    let old_image = decode_background_with_palette(&field, &old_palette, &BackgroundOptions::default()).unwrap();
    assert_eq!(image.bounds(), old_image.bounds());
    assert_eq!((image.width, image.height, image.view_bounds), (old_image.width, old_image.height, old_image.view_bounds));
    assert!(image.rgba.iter().skip(3).step_by(4).eq(old_image.rgba.iter().skip(3).step_by(4)), "the correction changes RGB only");
    let shown = image.flatten_on_black();
    let old_shown = old_image.flatten_on_black();
    // Match the existing owned-data tests' pre-1.88 baseline.
    #[allow(clippy::chunks_exact_to_as_chunks)]
    let differences = shown.chunks_exact(4).zip(old_shown.chunks_exact(4)).filter(|(a, b)| a != b).count();
    assert_eq!(differences, 1551, "station pixels corrected by the PC raw-zero colour rule");
}
