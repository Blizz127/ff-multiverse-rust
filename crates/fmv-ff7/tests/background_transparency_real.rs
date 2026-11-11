//! Owned-data regression for the reactor's background palette flags. Only small pixel and
//! metadata assertions are retained; files are opened read-only and no game assets are written.

use fmv_ff7::background::{decode_background, decode_background_tiles_with_depth};
use fmv_ff7::field::{FieldFile, BG_PALETTE_FLAGS};
use fmv_ff7::lgp::LgpArchive;
use std::path::PathBuf;

#[test]
#[ignore = "needs the owner's original FF7 field archive (FMV_FF7_DATA)"]
fn reactor_palette_flags_preserve_floor_pixels_in_flat_and_depth_backgrounds() {
    let Some(data) = std::env::var_os("FMV_FF7_DATA") else {
        eprintln!("FMV_FF7_DATA is needed; skipping");
        return;
    };
    let mut archive = LgpArchive::open_path(PathBuf::from(data).join("field/flevel.lgp")).expect("owned field archive");
    // Screen coordinates (x right, y down), the layer-1 tile/palette, expected RGB, and
    // visible differences from the old unconditional-index-zero rule. nmkin_1's first two
    // samples are the upper floor: palette 0 and palette 5 both explicitly disable the key.
    for (name, samples, changed) in [
        ("nmkin_1", &[(0u16, 0u8, -56, -200, [49, 49, 41]), (561, 5, -131, -87, [16, 24, 41])][..], 153),
        ("nmkin_2", &[(256, 2, -256, -256, [24, 16, 24])][..], 653),
        ("nmkin_3", &[(256, 1, -208, -256, [99, 82, 24])][..], 405),
        ("nmkin_4", &[(0, 0, -160, -192, [156, 99, 33])][..], 373),
    ] {
        let packed = archive.read(name).expect("reactor field");
        let field = FieldFile::parse(fmv_ff7::lzss::decompress(&packed).expect("field compression")).expect("field");
        let background = field.background().unwrap();
        let image = decode_background(&field).unwrap();
        let tiles = decode_background_tiles_with_depth(&field).unwrap();
        // Reproduce the old rule on an in-memory copy only, to identify exactly which
        // visible pixels the flags repair without changing the archive or its colours.
        let mut old_bytes = field.bytes().to_vec();
        let background_offset = u32::from_le_bytes(old_bytes[38..42].try_into().unwrap()) as usize;
        let flags = background_offset + 4 + 12;
        old_bytes[flags..flags + BG_PALETTE_FLAGS].fill(1);
        let old_field = FieldFile::parse(old_bytes).unwrap();
        let old_image = decode_background(&old_field).unwrap();
        let shown = image.flatten_on_black();
        let old_shown = old_image.flatten_on_black();
        // Keep this test compatible with the crate's pre-1.88 baseline, like real_data.rs.
        #[allow(clippy::chunks_exact_to_as_chunks)]
        let differences = shown.chunks_exact(4).zip(old_shown.chunks_exact(4)).filter(|(a, b)| a != b).count();
        assert_eq!(differences, changed, "{name}");
        for &(index, page, x, y, rgb) in samples {
            assert!(!background.palette_transparency[usize::from(page)], "{name} palette {page} is opaque");
            let raw_tile = background.tiles.iter().find(|tile| tile.layer == 1 && tile.index == index).unwrap();
            assert_eq!(raw_tile.palette_id, page);
            let (slot, sx, sy) = raw_tile.effective_source();
            let texture = background.texture(slot).unwrap();
            assert_eq!(texture.depth, 1);
            let (tx, ty) = (x - i32::from(raw_tile.dst_x), y - i32::from(raw_tile.dst_y));
            assert_eq!(texture.pixels[(usize::from(sy) + ty as usize) * 256 + usize::from(sx) + tx as usize], 0);
            let expected = [rgb[0], rgb[1], rgb[2], 255];
            assert_eq!(image.pixel((x + image.origin_x) as u32, (y + image.origin_y) as u32), Some(expected));
            assert_eq!(old_image.pixel((x + old_image.origin_x) as u32, (y + old_image.origin_y) as u32).unwrap()[3], 0);
            let tile = tiles.iter().find(|tile| tile.layer == 1 && tile.index == index).unwrap();
            assert_eq!(tile.pixel(tx as u32, ty as u32), Some(expected), "the depth renderer receives the same opaque pixel");
        }
    }
}
