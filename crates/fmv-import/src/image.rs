//! Disc image access: plain `.iso`, raw 2352-byte `.bin` and `.cue` sheets as [`SectorSource`]s.
//!
//! Images are opened read-only. Physical drives are out of scope for this increment.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::cue::{parse_cue, resolve_cue_file, CueError, TrackMode, MAX_CUE_BYTES};
use crate::iso9660::{Form2Sector, IsoError, SectorSource, SECTOR_SIZE};

pub const RAW_SECTOR: usize = 2352;
const SYNC: [u8; 12] = [
    0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// Plain 2048-byte sectors (`.iso`, or a cooked track).
    Cooked2048,
    /// Raw 2352-byte MODE1 sectors: sync(12) header(4) data(2048) edc/ecc(288).
    RawMode1,
    /// Raw 2352-byte MODE2 sectors (CD-XA): sync(12) header(4) subheader(8) data(2048) ...
    RawMode2,
}

impl Layout {
    pub fn sector_size(self) -> u64 {
        match self {
            Layout::Cooked2048 => SECTOR_SIZE as u64,
            Layout::RawMode1 | Layout::RawMode2 => RAW_SECTOR as u64,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Layout::Cooked2048 => "plain 2048-byte sectors",
            Layout::RawMode1 => "raw MODE1/2352",
            Layout::RawMode2 => "raw MODE2/2352",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ImageError {
    #[error("cannot read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{0}")]
    Cue(#[from] CueError),
    #[error("{0} is not a regular file")]
    NotAFile(PathBuf),
    #[error("{0} is too large to be a cue sheet")]
    CueTooLarge(PathBuf),
    #[error(
        "no ISO9660 volume found in {0}: tried plain 2048-byte, raw MODE2/2352 and raw MODE1/2352 sector layouts \
         (physical drives and other formats such as CHD are not supported in this increment)"
    )]
    NoIso(PathBuf),
}

fn io_at(path: &Path) -> impl FnOnce(io::Error) -> ImageError {
    let path = path.to_path_buf();
    move |source| ImageError::Io { path, source }
}

/// Read-only sector source over an image file.
#[derive(Debug)]
pub struct ImageSource {
    file: File,
    layout: Layout,
    start_sector: u64,
    sectors: u64,
}

impl ImageSource {
    fn new(file: File, layout: Layout, start_sector: u64) -> io::Result<Self> {
        let len = file.metadata()?.len();
        let sectors = (len / layout.sector_size()).saturating_sub(start_sector);
        Ok(ImageSource {
            file,
            layout,
            start_sector,
            sectors,
        })
    }

    pub fn layout(&self) -> Layout {
        self.layout
    }

    fn looks_like_iso(&mut self) -> bool {
        if self.sectors <= 16 {
            return false;
        }
        let mut sec = [0u8; SECTOR_SIZE];
        if self.read_sector(16, &mut sec).is_err() {
            return false;
        }
        let signature: &[u8] = &[1, b'C', b'D', b'0', b'0', b'1', 1];
        sec.get(..7) == Some(signature)
    }
}

impl SectorSource for ImageSource {
    fn sector_count(&self) -> u64 {
        self.sectors
    }

    fn read_sector(&mut self, lba: u64, out: &mut [u8; SECTOR_SIZE]) -> Result<(), IsoError> {
        let out_of_range = IsoError::OutOfRange {
            lba,
            sectors: self.sectors,
        };
        if lba >= self.sectors {
            return Err(out_of_range);
        }
        let pos = self
            .start_sector
            .checked_add(lba)
            .and_then(|s| s.checked_mul(self.layout.sector_size()))
            .ok_or(out_of_range)?;
        self.file.seek(SeekFrom::Start(pos))?;
        match self.layout {
            Layout::Cooked2048 => {
                self.file.read_exact(out)?;
                Ok(())
            }
            Layout::RawMode1 | Layout::RawMode2 => {
                let mut raw = [0u8; RAW_SECTOR];
                self.file.read_exact(&mut raw)?;
                if raw.get(..12) != Some(&SYNC[..]) {
                    return Err(IsoError::BadRawSector {
                        lba,
                        why: "missing sync pattern",
                    });
                }
                let (mode, data_off) = if self.layout == Layout::RawMode1 {
                    (1u8, 16usize)
                } else {
                    (2u8, 24usize)
                };
                if raw.get(15) != Some(&mode) {
                    return Err(IsoError::BadRawSector {
                        lba,
                        why: "unexpected sector mode byte",
                    });
                }
                if mode == 2 {
                    // submode bit 5: Form 2 (XA stream data) has no 2048-byte payload
                    let submode = raw.get(18).copied().unwrap_or(0);
                    if submode & 0x20 != 0 {
                        return Err(IsoError::Form2(Form2Sector { lba }));
                    }
                }
                let data =
                    raw.get(data_off..data_off + SECTOR_SIZE)
                        .ok_or(IsoError::BadRawSector {
                            lba,
                            why: "short sector",
                        })?;
                out.copy_from_slice(data);
                Ok(())
            }
        }
    }
}

/// An image opened for reading, with enough detail for reporting and for guarding the files
/// that were read.
#[derive(Debug)]
pub struct OpenedImage {
    pub source: ImageSource,
    /// The file that holds the sectors (the `.bin`/`.iso`).
    pub data_path: PathBuf,
    /// The `.cue` sheet, when one was given.
    pub cue_path: Option<PathBuf>,
    pub layout: Layout,
    /// Set when the cue sheet's declared mode disagreed with what the data actually looks like.
    pub note: Option<String>,
}

/// Open `path` (`.cue`, `.iso` or `.bin`). Without a cue sheet the sector layout is detected
/// by looking for the ISO9660 descriptor at sector 16.
pub fn open_image(path: &Path) -> Result<OpenedImage, ImageError> {
    let is_cue = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("cue"));
    if is_cue {
        open_cue(path)
    } else {
        open_data(path, None, 0, None)
    }
}

fn open_cue(cue_path: &Path) -> Result<OpenedImage, ImageError> {
    let meta = fs::metadata(cue_path).map_err(io_at(cue_path))?;
    if !meta.is_file() {
        return Err(ImageError::NotAFile(cue_path.to_path_buf()));
    }
    if meta.len() > MAX_CUE_BYTES {
        return Err(ImageError::CueTooLarge(cue_path.to_path_buf()));
    }
    let mut bytes = Vec::new();
    File::open(cue_path)
        .map_err(io_at(cue_path))?
        .take(MAX_CUE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(io_at(cue_path))?;
    if bytes.len() as u64 > MAX_CUE_BYTES {
        return Err(ImageError::CueTooLarge(cue_path.to_path_buf()));
    }
    let sheet = parse_cue(&String::from_utf8_lossy(&bytes))?;
    let track = sheet.data_track()?;
    let data_path = resolve_cue_file(cue_path, &track.file)?;
    let declared = match track.mode {
        TrackMode::Mode1Cooked => Layout::Cooked2048,
        TrackMode::Mode1Raw => Layout::RawMode1,
        TrackMode::Mode2Raw => Layout::RawMode2,
        // data_track() already rejects these
        TrackMode::Audio | TrackMode::Unsupported(_) => {
            return Err(CueError::NotData {
                number: track.number,
                mode: "unsupported".to_string(),
            }
            .into())
        }
    };
    open_data(
        &data_path,
        Some(declared),
        u64::from(track.index01_frames.unwrap_or(0)),
        Some(cue_path.to_path_buf()),
    )
}

fn open_data(
    path: &Path,
    declared: Option<Layout>,
    start_sector: u64,
    cue_path: Option<PathBuf>,
) -> Result<OpenedImage, ImageError> {
    let meta = fs::metadata(path).map_err(io_at(path))?;
    if !meta.is_file() {
        return Err(ImageError::NotAFile(path.to_path_buf()));
    }
    let mut candidates = vec![Layout::Cooked2048, Layout::RawMode2, Layout::RawMode1];
    if let Some(d) = declared {
        candidates.retain(|c| *c != d);
        candidates.insert(0, d);
    }
    for layout in candidates {
        let file = File::open(path).map_err(io_at(path))?;
        let mut source = ImageSource::new(file, layout, start_sector).map_err(io_at(path))?;
        if source.looks_like_iso() {
            let note = match declared {
                Some(d) if d != layout => Some(format!(
                    "the cue sheet declares {} but the data reads as {}; using {}",
                    d.label(),
                    layout.label(),
                    layout.label()
                )),
                _ => None,
            };
            return Ok(OpenedImage {
                source,
                data_path: path.to_path_buf(),
                cue_path,
                layout,
                note,
            });
        }
    }
    Err(ImageError::NoIso(path.to_path_buf()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iso9660::Volume;
    use crate::synthetic::{
        build_iso, cue_for, lba_of, mark_form2, system_cnf, wrap_raw, RawMode, TEST_PAYLOAD_A,
        TEST_PAYLOAD_B,
    };
    use std::io::Read;

    fn iso() -> Vec<u8> {
        build_iso(&[
            ("SYSTEM.CNF", &system_cnf("SCUS_941.63")),
            ("FIELD/MD1STIN.DAT", TEST_PAYLOAD_A),
            ("BATTLE/SCENE.BIN", TEST_PAYLOAD_B),
        ])
    }

    /// Open `image`, list files, read every file, and compare with the originals.
    fn check_reads(image: &Path) -> OpenedImage {
        let opened = open_image(image).unwrap();
        let layout = opened.layout;
        let mut vol = Volume::open(opened.source).unwrap();
        let walk = vol.walk().unwrap();
        assert!(walk.skipped.is_empty(), "{:?}", walk.skipped);
        assert_eq!(walk.files.len(), 3, "{layout:?}");
        for (path, expect) in [
            ("FIELD/MD1STIN.DAT", TEST_PAYLOAD_A),
            ("BATTLE/SCENE.BIN", TEST_PAYLOAD_B),
        ] {
            let f = walk.files.iter().find(|f| f.path == path).unwrap();
            let mut got = Vec::new();
            vol.file_reader(f).unwrap().read_to_end(&mut got).unwrap();
            assert_eq!(got, expect, "{path} via {layout:?}");
        }
        let cnf = walk
            .files
            .iter()
            .find(|f| f.path == "SYSTEM.CNF")
            .unwrap()
            .clone();
        let bytes = vol.read_file_limited(&cnf, 4096).unwrap();
        assert_eq!(
            crate::serial::parse_boot_serial(&bytes).unwrap().serial,
            "SCUS-94163"
        );
        // reopen for the caller (the source moved into the volume)
        open_image(image).unwrap()
    }

    #[test]
    fn plain_iso_and_raw_bins_all_read_identically() {
        let dir = tempfile::tempdir().unwrap();
        let cooked = iso();
        let m2 = wrap_raw(&cooked, RawMode::Mode2Form1);
        let m1 = wrap_raw(&cooked, RawMode::Mode1);

        std::fs::write(dir.path().join("plain.iso"), &cooked).unwrap();
        assert_eq!(
            check_reads(&dir.path().join("plain.iso")).layout,
            Layout::Cooked2048
        );

        // raw .bin without a cue: layout is detected
        std::fs::write(dir.path().join("m2.bin"), &m2).unwrap();
        assert_eq!(
            check_reads(&dir.path().join("m2.bin")).layout,
            Layout::RawMode2
        );
        std::fs::write(dir.path().join("m1.bin"), &m1).unwrap();
        assert_eq!(
            check_reads(&dir.path().join("m1.bin")).layout,
            Layout::RawMode1
        );

        // with cue sheets (declared modes match)
        std::fs::write(dir.path().join("d2.cue"), cue_for("m2.bin", "MODE2/2352")).unwrap();
        let o = check_reads(&dir.path().join("d2.cue"));
        assert_eq!((o.layout, o.note.is_none()), (Layout::RawMode2, true));
        assert_eq!(o.data_path, dir.path().join("m2.bin"));
        assert_eq!(
            o.cue_path.as_deref(),
            Some(dir.path().join("d2.cue").as_path())
        );
        std::fs::write(dir.path().join("d1.CUE"), cue_for("m1.bin", "MODE1/2352")).unwrap();
        assert_eq!(
            check_reads(&dir.path().join("d1.CUE")).layout,
            Layout::RawMode1
        );
        std::fs::write(dir.path().join("c.cue"), cue_for("plain.iso", "MODE1/2048")).unwrap();
        assert_eq!(
            check_reads(&dir.path().join("c.cue")).layout,
            Layout::Cooked2048
        );

        // a cue that mislabels the mode still opens, with a note
        std::fs::write(dir.path().join("lie.cue"), cue_for("m2.bin", "MODE1/2352")).unwrap();
        let o = open_image(&dir.path().join("lie.cue")).unwrap();
        assert_eq!(o.layout, Layout::RawMode2);
        assert!(o.note.unwrap().contains("declares"));

        // a Windows-style absolute path in the cue resolves to the file next to it
        std::fs::write(
            dir.path().join("win.cue"),
            cue_for("C:\\isos\\ff7\\m2.bin", "MODE2/2352"),
        )
        .unwrap();
        assert_eq!(
            open_image(&dir.path().join("win.cue")).unwrap().data_path,
            dir.path().join("m2.bin")
        );
    }

    #[test]
    fn raw_sector_layout_is_exactly_sync_header_subheader_data() {
        let cooked = iso();
        let m2 = wrap_raw(&cooked, RawMode::Mode2Form1);
        assert_eq!(m2.len(), cooked.len() / 2048 * 2352);
        let s16 = &m2[16 * 2352..17 * 2352];
        assert_eq!(&s16[..12], &SYNC);
        assert_eq!(s16[15], 2);
        assert_eq!(s16[18] & 0x20, 0, "Form 1");
        assert_eq!(&s16[24..24 + 7], &[1, b'C', b'D', b'0', b'0', b'1', 1]);
        // MSF header of sector 16 = 16 + 150 frames = 00:02:16 in BCD
        assert_eq!(&s16[12..15], &[0x00, 0x02, 0x16]);
    }

    #[test]
    fn form2_sectors_are_reported_not_misread() {
        let dir = tempfile::tempdir().unwrap();
        let cooked = iso();
        let lba = lba_of(&cooked, "BATTLE/SCENE.BIN").unwrap() as usize;
        let mut raw = wrap_raw(&cooked, RawMode::Mode2Form1);
        mark_form2(&mut raw, lba);
        std::fs::write(dir.path().join("xa.bin"), &raw).unwrap();
        let opened = open_image(&dir.path().join("xa.bin")).unwrap();
        let mut vol = Volume::open(opened.source).unwrap();
        let walk = vol.walk().unwrap();
        let f = walk
            .files
            .iter()
            .find(|f| f.path == "BATTLE/SCENE.BIN")
            .unwrap()
            .clone();
        let mut sink = Vec::new();
        let err = vol
            .file_reader(&f)
            .unwrap()
            .read_to_end(&mut sink)
            .unwrap_err();
        let form2 = err.get_ref().and_then(|e| e.downcast_ref::<Form2Sector>());
        assert_eq!(form2, Some(&Form2Sector { lba: lba as u64 }), "{err}");
        // untouched files still read fine
        let ok = walk
            .files
            .iter()
            .find(|f| f.path == "FIELD/MD1STIN.DAT")
            .unwrap()
            .clone();
        let mut good = Vec::new();
        vol.file_reader(&ok)
            .unwrap()
            .read_to_end(&mut good)
            .unwrap();
        assert_eq!(good, TEST_PAYLOAD_A);
    }

    #[test]
    fn bad_inputs_are_errors_not_panics() {
        let dir = tempfile::tempdir().unwrap();
        let p = |n: &str| dir.path().join(n);
        // empty, tiny and garbage files
        std::fs::write(p("empty.iso"), b"").unwrap();
        std::fs::write(p("tiny.bin"), vec![0u8; 100]).unwrap();
        std::fs::write(p("zeros.bin"), vec![0u8; 20 * 2352]).unwrap();
        for f in ["empty.iso", "tiny.bin", "zeros.bin"] {
            assert!(
                matches!(open_image(&p(f)), Err(ImageError::NoIso(_))),
                "{f}"
            );
        }
        // missing file, directory, missing bin behind a cue
        assert!(matches!(
            open_image(&p("nope.iso")),
            Err(ImageError::Io { .. })
        ));
        assert!(matches!(
            open_image(dir.path()),
            Err(ImageError::NotAFile(_))
        ));
        std::fs::write(p("gone.cue"), cue_for("missing.bin", "MODE2/2352")).unwrap();
        assert!(matches!(
            open_image(&p("gone.cue")),
            Err(ImageError::Io { .. })
        ));
        // cue problems
        std::fs::write(
            p("audio.cue"),
            "FILE a.bin BINARY\nTRACK 01 AUDIO\nINDEX 01 00:00:00\n",
        )
        .unwrap();
        assert!(matches!(
            open_image(&p("audio.cue")),
            Err(ImageError::Cue(_))
        ));
        std::fs::write(p("junk.cue"), b"\xff\xfe\x00garbage").unwrap();
        assert!(matches!(
            open_image(&p("junk.cue")),
            Err(ImageError::Cue(_))
        ));
        let huge = vec![b' '; (MAX_CUE_BYTES + 1) as usize];
        std::fs::write(p("huge.cue"), huge).unwrap();
        assert!(matches!(
            open_image(&p("huge.cue")),
            Err(ImageError::CueTooLarge(_))
        ));

        // a raw image whose sync bytes are damaged after the PVD sector
        let cooked = iso();
        let mut raw = wrap_raw(&cooked, RawMode::Mode2Form1);
        let lba = lba_of(&cooked, "FIELD/MD1STIN.DAT").unwrap() as usize;
        raw[lba * 2352 + 3] = 0x12;
        std::fs::write(p("badsync.bin"), &raw).unwrap();
        let opened = open_image(&p("badsync.bin")).unwrap();
        let mut vol = Volume::open(opened.source).unwrap();
        let walk = vol.walk().unwrap();
        let f = walk
            .files
            .iter()
            .find(|f| f.path == "FIELD/MD1STIN.DAT")
            .unwrap()
            .clone();
        let mut sink = Vec::new();
        assert!(vol.file_reader(&f).unwrap().read_to_end(&mut sink).is_err());
        // a truncated image: the files beyond the end are skipped by the walker
        let cut = wrap_raw(&cooked, RawMode::Mode2Form1);
        std::fs::write(p("cut.bin"), &cut[..(lba) * 2352]).unwrap();
        let opened = open_image(&p("cut.bin")).unwrap();
        let mut vol = Volume::open(opened.source).unwrap();
        let walk = vol.walk().unwrap();
        assert!(
            walk.skipped.iter().any(|s| s.reason.contains("outside")),
            "{:?}",
            walk.skipped
        );
    }
}
