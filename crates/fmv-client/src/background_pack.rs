//! Optional, local replacement texture pages for the `nmkin_1` field trial.
//!
//! The pack replaces pixels only: the field's own tiles still supply source coordinates, depth,
//! blending and visibility. A field is loaded atomically from five 4x pages. These pages describe
//! its original palette, so any palette change selects the entire native field for that frame;
//! this pilot does not pretend to reproduce palette animation with a static texture.
//!
//! Nothing is enabled by default. `FMV_BACKGROUND_DIR` names an explicitly supplied private pack
//! root; `open` takes the same root without changing process-wide state, including in tests.

use fmv_ff7::field::Palette;
use std::fmt;
use std::fs::File;
use std::io::{self, Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const BACKGROUND_DIR_ENV: &str = "FMV_BACKGROUND_DIR";
const FIELD: &str = "nmkin_1";
const PAGES: [u8; 5] = [0, 1, 2, 3, 15];
const SCALE: u32 = 4;
const PAGE_SIDE: u32 = 256 * SCALE;
const PAGE_BYTES: usize = PAGE_SIDE as usize * PAGE_SIDE as usize * 4;
const MAX_ENCODED_BYTES: u64 = 8 * 1024 * 1024;
const DECODER_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum PackError {
    #[error("background pack: invalid field key")]
    InvalidField,
    #[error("background pack: root must be a directory")]
    InvalidRoot,
    #[error("background pack: a page resolves outside its root")]
    OutsideRoot,
    #[error("background pack: a page is not a regular file")]
    NotFile,
    #[error("background pack: encoded page exceeds the 8 MiB limit")]
    TooLarge,
    #[error("background pack: pages must be static 1024 x 1024 RGBA8 PNGs")]
    WrongFormat,
    #[error("background pack: {0}")]
    Io(#[from] io::Error),
    #[error("background pack: invalid PNG ({0})")]
    Png(#[from] png::DecodingError),
}

/// One immutable source page, still containing the original decoded alpha values.
/// Rendering applies its own alpha threshold consistently to tiles and the flat backdrop.
pub struct RgbaPage {
    pub side: u32,
    pub rgba: Arc<[u8]>,
}

/// `Debug` describes the allocation; it never prints the owner's image data.
impl fmt::Debug for RgbaPage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RgbaPage")
            .field("side", &self.side)
            .field("bytes", &self.rgba.len())
            .finish()
    }
}

/// A complete field's replacement pixels and the original palette they were made for.
/// Its five pages retain exactly 20 MiB of decoded pixels. Page access requires a palette check.
#[derive(Debug)]
pub struct FieldTextureSet {
    baseline: Palette,
    pages: Vec<(u8, RgbaPage)>,
}

impl FieldTextureSet {
    pub fn scale(&self) -> u32 {
        SCALE
    }

    /// All pages, only when the current palette is exactly the field's original palette.
    /// Checking the whole palette is deliberately conservative: a change in any page falls back.
    pub fn for_palette(&self, current: &Palette) -> Option<FramePages<'_>> {
        (current == &self.baseline).then_some(FramePages { set: self })
    }
}

/// Borrowed pages eligible for a single frame. The renderer still verifies that every effective
/// source page and crop required by that frame exists before using any replacement pixels.
pub struct FramePages<'a> {
    set: &'a FieldTextureSet,
}

impl FramePages<'_> {
    pub fn page(&self, source_page: u8) -> Option<&RgbaPage> {
        self.set
            .pages
            .iter()
            .find(|(id, _)| *id == source_page)
            .map(|(_, page)| page)
    }
}

#[derive(Debug)]
pub struct PngFieldPack {
    root: PathBuf,
}

impl PngFieldPack {
    pub fn from_env() -> Result<Option<Self>, PackError> {
        std::env::var_os(BACKGROUND_DIR_ENV)
            .map(|root| Self::open(Path::new(&root)))
            .transpose()
    }

    /// Open a local root without mutating the environment or reading any image yet.
    pub fn open(root: &Path) -> Result<Self, PackError> {
        if root.as_os_str().is_empty() {
            return Err(PackError::InvalidRoot);
        }
        let root = root.canonicalize()?;
        if !root.is_dir() {
            return Err(PackError::InvalidRoot);
        }
        Ok(Self { root })
    }

    /// Load the pilot field completely or return `None` when it is not supplied. A malformed or
    /// unsafe file is an error; callers can report it once and keep the native field. Files are
    /// derived from a validated key, confined to the canonical root, and decoded one at a time.
    pub fn load_field(
        &self,
        key: &str,
        baseline: &Palette,
    ) -> Result<Option<FieldTextureSet>, PackError> {
        if key.is_empty()
            || key.len() > 32
            || !key
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        {
            return Err(PackError::InvalidField);
        }
        if key != FIELD {
            return Ok(None);
        }
        // Check completeness before allocating any decoded page. Never hand out a partial set.
        let mut paths = Vec::with_capacity(PAGES.len());
        for page in PAGES {
            let path = self
                .root
                .join("field")
                .join(key)
                .join(format!("{key}_{page:02}_00.png"));
            let path = match path.canonicalize() {
                Ok(path) => path,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(e.into()),
            };
            if !path.starts_with(&self.root) {
                return Err(PackError::OutsideRoot);
            }
            let metadata = path.metadata()?;
            if !metadata.is_file() {
                return Err(PackError::NotFile);
            }
            if metadata.len() > MAX_ENCODED_BYTES {
                return Err(PackError::TooLarge);
            }
            paths.push((page, path));
        }
        let mut pages = Vec::with_capacity(PAGES.len());
        for (page, path) in paths {
            let file = File::open(path)?;
            let metadata = file.metadata()?;
            if !metadata.is_file() {
                return Err(PackError::NotFile);
            }
            if metadata.len() > MAX_ENCODED_BYTES {
                return Err(PackError::TooLarge);
            }
            let mut encoded = Vec::with_capacity(metadata.len() as usize);
            file.take(MAX_ENCODED_BYTES + 1).read_to_end(&mut encoded)?;
            if encoded.len() as u64 > MAX_ENCODED_BYTES {
                return Err(PackError::TooLarge);
            }
            pages.push((page, decode_page(&encoded)?));
        }
        Ok(Some(FieldTextureSet {
            baseline: baseline.clone(),
            pages,
        }))
    }
}

fn correct_format(info: &png::Info<'_>) -> bool {
    info.width == PAGE_SIDE
        && info.height == PAGE_SIDE
        && info.color_type == png::ColorType::Rgba
        && info.bit_depth == png::BitDepth::Eight
        && info.animation_control.is_none()
        && info.frame_control.is_none()
}

fn decode_page(encoded: &[u8]) -> Result<RgbaPage, PackError> {
    if encoded.len() as u64 > MAX_ENCODED_BYTES {
        return Err(PackError::TooLarge);
    }
    let mut decoder = png::Decoder::new(Cursor::new(encoded));
    decoder.set_limits(png::Limits {
        bytes: DECODER_BYTES,
    });
    decoder.set_ignore_text_chunk(true);
    decoder.set_ignore_iccp_chunk(true);
    // Check dimensions immediately after IHDR, before scanline or output buffers are allocated.
    if !correct_format(decoder.read_header_info()?) {
        return Err(PackError::WrongFormat);
    }
    let mut reader = decoder.read_info()?;
    // Animation metadata occurs after IHDR; reject it here as well.
    if !correct_format(reader.info()) || reader.output_buffer_size() != Some(PAGE_BYTES) {
        return Err(PackError::WrongFormat);
    }
    let mut rgba = vec![0; PAGE_BYTES];
    let output = reader.next_frame(&mut rgba)?;
    if output.width != PAGE_SIDE
        || output.height != PAGE_SIDE
        || output.color_type != png::ColorType::Rgba
        || output.bit_depth != png::BitDepth::Eight
        || output.buffer_size() != PAGE_BYTES
    {
        return Err(PackError::WrongFormat);
    }
    // Check the rest of the stream too, including CRCs and the final IEND chunk.
    reader.finish()?;
    Ok(RgbaPage {
        side: PAGE_SIDE,
        rgba: rgba.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn palette() -> Palette {
        let mut bytes = vec![0; 8];
        bytes.extend_from_slice(&256u16.to_le_bytes());
        bytes.extend_from_slice(&14u16.to_le_bytes());
        bytes.resize(12 + 14 * 256 * 2, 0);
        Palette::parse(&bytes).unwrap()
    }

    fn png_bytes(side: u32, color: png::ColorType) -> Vec<u8> {
        let mut encoded = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut encoded, side, side);
            encoder.set_color(color);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            let pixel = if color == png::ColorType::Rgba {
                &[11, 22, 33, 127][..]
            } else {
                &[11, 22, 33][..]
            };
            writer
                .write_image_data(&pixel.repeat(side as usize * side as usize))
                .unwrap();
            writer.finish().unwrap();
        }
        encoded
    }

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("field").join(FIELD);
        fs::create_dir_all(&folder).unwrap();
        let encoded = png_bytes(PAGE_SIDE, png::ColorType::Rgba);
        for page in PAGES {
            fs::write(folder.join(format!("{FIELD}_{page:02}_00.png")), &encoded).unwrap();
        }
        dir
    }

    fn first(root: &Path) -> PathBuf {
        root.join("field").join(FIELD).join("nmkin_1_00_00.png")
    }

    #[test]
    fn complete_pages_preserve_rgba_and_palette_changes_fall_back_as_one_field() {
        let dir = fixture();
        let pack = PngFieldPack::open(dir.path()).unwrap();
        let baseline = palette();
        let set = pack.load_field(FIELD, &baseline).unwrap().unwrap();
        assert_eq!(set.scale(), 4);
        let frame = set.for_palette(&baseline).unwrap();
        for page in PAGES {
            let page = frame.page(page).unwrap();
            assert_eq!((page.side, page.rgba.len()), (1024, PAGE_BYTES));
            assert!(
                page.rgba
                    .as_chunks::<4>().0.iter()
                    .all(|pixel| *pixel == [11, 22, 33, 127]),
                "PNG loading must not alter color or alpha"
            );
        }
        assert!(frame.page(4).is_none());
        let mut changed = baseline.clone();
        assert_eq!(changed.set_raw(13, 255, 123), Some(true));
        assert!(
            set.for_palette(&changed).is_none(),
            "a changed effect palette invalidates every replacement page"
        );
        changed.set_raw(13, 255, 0);
        assert!(
            set.for_palette(&changed).is_some(),
            "restoring the original palette restores eligibility"
        );
        changed.declared_pages += 1;
        assert!(
            set.for_palette(&changed).is_none(),
            "palette layout must match too"
        );
        assert!(
            format!("{set:?}").len() < 1000,
            "Debug must not dump pixels"
        );
    }

    #[test]
    fn missing_page_or_unsupported_field_never_produces_a_partial_set() {
        let dir = fixture();
        let pack = PngFieldPack::open(dir.path()).unwrap();
        fs::remove_file(dir.path().join("field/nmkin_1/nmkin_1_15_00.png")).unwrap();
        assert!(pack.load_field(FIELD, &palette()).unwrap().is_none());
        assert!(pack.load_field("md1stin", &palette()).unwrap().is_none());
    }

    #[test]
    fn invalid_keys_and_roots_are_refused_before_lookup() {
        let dir = tempfile::tempdir().unwrap();
        let pack = PngFieldPack::open(dir.path()).unwrap();
        for key in [
            "",
            "../nmkin_1",
            "/nmkin_1",
            "field/nmkin_1",
            "field\\nmkin_1",
            "nmkin_1\0",
            "NMKIN_1",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ] {
            assert!(
                matches!(
                    pack.load_field(key, &palette()),
                    Err(PackError::InvalidField)
                ),
                "{key:?}"
            );
        }
        fs::write(dir.path().join("file"), b"x").unwrap();
        assert!(matches!(
            PngFieldPack::open(&dir.path().join("file")),
            Err(PackError::InvalidRoot)
        ));
        assert!(matches!(
            PngFieldPack::open(Path::new("")),
            Err(PackError::InvalidRoot)
        ));
    }

    #[test]
    fn corrupt_oversized_and_non_file_pages_are_refused() {
        let dir = fixture();
        let pack = PngFieldPack::open(dir.path()).unwrap();
        let path = first(dir.path());
        fs::write(&path, b"not a PNG").unwrap();
        assert!(matches!(
            pack.load_field(FIELD, &palette()),
            Err(PackError::Png(_))
        ));
        File::create(&path)
            .unwrap()
            .set_len(MAX_ENCODED_BYTES + 1)
            .unwrap();
        assert!(matches!(
            pack.load_field(FIELD, &palette()),
            Err(PackError::TooLarge)
        ));
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(matches!(
            pack.load_field(FIELD, &palette()),
            Err(PackError::NotFile)
        ));
    }

    #[test]
    fn wrong_dimensions_and_rgb_pages_are_refused_without_conversion() {
        assert!(matches!(
            decode_page(&png_bytes(16, png::ColorType::Rgba)),
            Err(PackError::WrongFormat)
        ));
        assert!(matches!(
            decode_page(&png_bytes(PAGE_SIDE, png::ColorType::Rgb)),
            Err(PackError::WrongFormat)
        ));
        // A huge IHDR alone is enough to reject an image; no full-size image is allocated.
        let mut header = Vec::new();
        png::Encoder::new(&mut header, 1_000_000, 1_000_000)
            .write_header()
            .unwrap();
        assert!(matches!(decode_page(&header), Err(PackError::WrongFormat)));
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_cannot_escape_the_pack_root() {
        use std::os::unix::fs::symlink;
        let dir = fixture();
        let elsewhere = tempfile::tempdir().unwrap();
        let outside = elsewhere.path().join("outside.png");
        fs::write(&outside, png_bytes(PAGE_SIDE, png::ColorType::Rgba)).unwrap();
        let path = first(dir.path());
        fs::remove_file(&path).unwrap();
        symlink(&outside, &path).unwrap();
        let pack = PngFieldPack::open(dir.path()).unwrap();
        assert!(matches!(
            pack.load_field(FIELD, &palette()),
            Err(PackError::OutsideRoot)
        ));
    }

    #[test]
    #[ignore = "needs the private converted SYW nmkin_1 pages (FMV_BACKGROUND_TEST_DIR)"]
    fn the_private_trial_loads_all_five_real_pages_as_rgba8_at_four_times_native_size() {
        let Some(root) = std::env::var_os("FMV_BACKGROUND_TEST_DIR") else {
            eprintln!("FMV_BACKGROUND_TEST_DIR is needed; skipping");
            return;
        };
        let pack = PngFieldPack::open(Path::new(&root)).unwrap();
        let baseline = palette();
        let set = pack
            .load_field(FIELD, &baseline)
            .unwrap()
            .expect("all five pages");
        let frame = set.for_palette(&baseline).unwrap();
        assert_eq!(set.scale(), 4);
        for page in PAGES {
            let image = frame.page(page).unwrap();
            assert_eq!((image.side, image.rgba.len()), (1024, 4 * 1024 * 1024));
            assert!(image.rgba.as_chunks::<4>().0.iter().any(|pixel| pixel[3] > 0));
        }
    }
}
