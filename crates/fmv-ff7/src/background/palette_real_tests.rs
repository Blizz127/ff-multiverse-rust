//! Owned-data regression for the map lesson's palette buffers and both background paths.
//! Only numerical metadata and comparisons are retained; no game assets are written.

use super::depth::{decode_background_tiles_with_depth_palette, DepthTile};
use super::{decode_background_with_palette, BackgroundImage, BackgroundOp, BackgroundOptions, BackgroundState, GroupFilter, PaletteOp, PaletteState};
use crate::field::FieldFile;
use crate::lgp::LgpArchive;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

struct Ramp {
    source_palette: u8,
    source: u8,
    destination: u8,
    palette: u8,
    count: u16,
    group: u8,
    frame: u8,
    raw_tiles: usize,
    used_indices: usize,
    max_index: u8,
    depth_tiles: usize,
    affected_depth_tiles: usize,
    factors: &'static [u8],
    changes: &'static [usize],
}

/// Paint the exported depth quads in their supplied order over a transparent canvas, using
/// their own texels/blend modes. This compares the two consumer paths, including depth tiles
/// hidden by another tile in the final flat picture.
fn paint_depth(image: &BackgroundImage, tiles: &[DepthTile]) -> Vec<u8> {
    let mut canvas = vec![0; image.rgba.len()];
    for tile in tiles {
        for y in 0..tile.size() {
            for x in 0..tile.size() {
                let pixel = tile.pixel(x, y).expect("tile texel");
                if pixel[3] == 0 { continue; }
                let cx = tile.rect.min_x + x as i32 + image.origin_x;
                let cy = tile.rect.min_y + y as i32 + image.origin_y;
                assert!(cx >= 0 && cy >= 0 && cx < image.width as i32 && cy < image.height as i32);
                let at = (cy as usize * image.width as usize + cx as usize) * 4;
                let mut shown = pixel;
                if let Some(blend) = tile.blend {
                    for channel in 0..3 {
                        let behind = if canvas[at + 3] == 0 { 0 } else { canvas[at + channel] };
                        shown[channel] = blend.apply(behind, pixel[channel]);
                    }
                }
                shown[3] = 255;
                canvas[at..at + 4].copy_from_slice(&shown);
            }
        }
    }
    canvas
}

#[test]
#[ignore = "needs the owner's original FF7 field archive (FMV_FF7_DATA)"]
fn rootmap_authored_palette_ramps_change_the_expected_pixels_in_both_background_paths() {
    let Some(data) = std::env::var_os("FMV_FF7_DATA") else {
        eprintln!("FMV_FF7_DATA is needed; skipping");
        return;
    };
    let mut archive = LgpArchive::open_path(PathBuf::from(data).join("field/flevel.lgp")).expect("owned field archive");
    let packed = archive.read("rootmap").expect("rootmap");
    let field = FieldFile::parse(crate::lzss::decompress(&packed).expect("rootmap compression")).expect("rootmap field");
    let original = field.palette().expect("original palette");
    let background = field.background().expect("original background");
    assert_eq!((original.page_count(), original.colors_per_page, background.tiles.len()), (15, 256, 1_004));
    let by_tile: BTreeMap<_, _> = background.tiles.iter().map(|tile| ((tile.layer, tile.index), tile)).collect();

    // These source/buffer/destination/count tuples are the original monitor and shade scripts.
    // Positions count 16-color scratch blocks; the native counts 127/63 resolve to 128/64.
    // Each picture selects the group containing the destination palette so it can be checked
    // independently of dialogue duration and concurrent-script arbitration. Palette 0 is layer 1;
    // group 1/frame 0 leaves only ten of its changing pixels exposed, which is still observable.
    let ramps = [
        Ramp { source_palette: 7, source: 0, destination: 8, palette: 0, count: 128, group: 1, frame: 0,
            raw_tiles: 120, used_indices: 4, max_index: 3, depth_tiles: 380, affected_depth_tiles: 40,
            factors: &[0, 24, 44, 64], changes: &[10, 10, 10] },
        Ramp { source_palette: 3, source: 16, destination: 24, palette: 3, count: 128, group: 1, frame: 0,
            raw_tiles: 120, used_indices: 41, max_index: 40, depth_tiles: 380, affected_depth_tiles: 120,
            factors: &[0, 24, 44, 64], changes: &[23_013, 24_208, 24_312] },
        Ramp { source_palette: 8, source: 32, destination: 36, palette: 8, count: 64, group: 2, frame: 0,
            raw_tiles: 5, used_indices: 15, max_index: 14, depth_tiles: 265, affected_depth_tiles: 5,
            factors: &[0, 24, 44, 64], changes: &[241, 241, 241] },
        Ramp { source_palette: 12, source: 40, destination: 44, palette: 12, count: 64, group: 2, frame: 1,
            raw_tiles: 9, used_indices: 15, max_index: 14, depth_tiles: 269, affected_depth_tiles: 9,
            factors: &[0, 24, 44, 64], changes: &[238, 238, 238] },
        // The ambient shade loops range between 24 and 66 and do not begin with black.
        Ramp { source_palette: 9, source: 48, destination: 52, palette: 9, count: 64, group: 3, frame: 0,
            raw_tiles: 2, used_indices: 15, max_index: 14, depth_tiles: 262, affected_depth_tiles: 2,
            factors: &[24, 44, 64], changes: &[137, 145] },
        Ramp { source_palette: 14, source: 56, destination: 60, palette: 14, count: 64, group: 5, frame: 7,
            raw_tiles: 4, used_indices: 31, max_index: 30, depth_tiles: 264, affected_depth_tiles: 4,
            factors: &[24, 44, 64], changes: &[206, 257] },
    ];
    for ramp in ramps {
        let raw: Vec<_> = background.tiles.iter().filter(|tile| tile.palette_id == ramp.palette).collect();
        assert_eq!(raw.len(), ramp.raw_tiles, "palette {} raw tile inventory", ramp.palette);
        let mut indices = BTreeSet::new();
        for tile in &raw {
            let (slot, sx, sy) = tile.effective_source();
            let texture = background.textures[usize::from(slot)].as_ref().expect("tile's texture page");
            assert_eq!(texture.depth, 1, "the lesson's textures use 8-bit palette indices");
            for y in 0..usize::from(tile.tile_size) {
                for x in 0..usize::from(tile.tile_size) {
                    let index = texture.pixels[(usize::from(sy) + y) * 256 + usize::from(sx) + x];
                    assert!(u16::from(index) < ramp.count, "palette {} index {index} lies outside its script upload", ramp.palette);
                    indices.insert(index);
                }
            }
        }
        assert_eq!((indices.len(), indices.last().copied()), (ramp.used_indices, Some(ramp.max_index)));
        let mut groups = BackgroundState::default();
        groups.apply(BackgroundOp::On { group: ramp.group, frame: ramp.frame });
        let options = BackgroundOptions { groups: GroupFilter::State(groups), ..BackgroundOptions::default() };
        let mut palette = PaletteState::new(original.clone());
        assert!(!palette.apply(PaletteOp::Store { palette: ramp.source_palette, position: ramp.source, count: ramp.count }).unwrap());
        assert_eq!(palette.palette(), &original, "STPAL changes scratch only");
        let mut previous: Option<Vec<u8>> = None;
        let mut changes = Vec::new();
        for &factor in ramp.factors {
            let live_before = palette.palette().clone();
            assert!(!palette.apply(PaletteOp::Multiply {
                source: ramp.source, destination: ramp.destination, red: factor, green: factor, blue: factor, count: ramp.count,
            }).unwrap());
            // Every original ramp restores precisely entry zero (encoded CPPAL count 0).
            assert!(!palette.apply(PaletteOp::Copy { source: ramp.source, destination: ramp.destination, count: 1 }).unwrap());
            assert_eq!(palette.palette(), &live_before, "scratch operations do not publish colors");
            palette.apply(PaletteOp::Load { position: ramp.destination, palette: ramp.palette, count: ramp.count }).unwrap();
            assert_eq!(palette.palette().raw(usize::from(ramp.palette), 0), original.raw(usize::from(ramp.source_palette), 0));
            assert_eq!(palette.palette().rgba(usize::from(ramp.palette), 0).unwrap()[3], 0, "PC color-key transparency is unchanged");
            for page in 0..original.page_count() {
                for index in 0..usize::from(original.colors_per_page) {
                    if page != usize::from(ramp.palette) || index >= usize::from(ramp.count) {
                        assert_eq!(palette.palette().raw(page, index), original.raw(page, index), "partial upload changed unrelated color {page}:{index}");
                    }
                }
            }
            let flat = decode_background_with_palette(&field, palette.palette(), &options).unwrap();
            let tiles = decode_background_tiles_with_depth_palette(&field, palette.palette(), &options).unwrap();
            assert_eq!(tiles.len(), ramp.depth_tiles, "palette {} rendered tile inventory", ramp.palette);
            assert_eq!(tiles.iter().filter(|tile| by_tile[&(tile.layer, tile.index)].palette_id == ramp.palette).count(), ramp.affected_depth_tiles);
            for tile in &tiles {
                let source = by_tile[&(tile.layer, tile.index)];
                let pixels: Vec<u8> = background.tile_rgba(source, palette.palette()).unwrap().into_iter().flatten().collect();
                assert_eq!(tile.rgba, pixels, "depth tile {}:{} must use the live palette", tile.layer, tile.index);
            }
            assert_eq!(paint_depth(&flat, &tiles), flat.rgba, "palette {} scale {factor}: flat and depth colors agree", ramp.palette);
            if let Some(before) = previous {
                let changed = before.chunks_exact(4).zip(flat.rgba.chunks_exact(4)).filter(|(old, new)| old != new).count();
                changes.push(changed);
            }
            previous = Some(flat.rgba);
        }
        assert_eq!(changes, ramp.changes, "palette {} visible changes between sampled authored factors", ramp.palette);
        // Scale 64 recovers the original stored source; subsequent ramps did not overwrite it.
        for index in 0..usize::from(ramp.count) {
            assert_eq!(palette.palette().raw(usize::from(ramp.palette), index), original.raw(usize::from(ramp.source_palette), index));
        }
        eprintln!("rootmap palette {}: {} source tiles, {} rendered tiles, pixel changes {changes:?}", ramp.palette, ramp.raw_tiles, ramp.affected_depth_tiles);
    }
}
