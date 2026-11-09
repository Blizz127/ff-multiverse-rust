//! A check of the audio discovery against the owner's real files, `#[ignore]`d: it only runs when
//! asked for, with the folders named in the environment:
//!
//! ```text
//! FMV_FF7_AUDIO_DATA=<the data/ folder that holds music_ogg/, music/ and sound/> \
//! FMV_FF7_DATA=<the data/ folder that holds field/ and battle/> \
//! cargo test -p fmv-import --test real_audio -- --ignored --nocapture
//! ```
//!
//! Both folders are read-only here: names and sizes are listed, nothing is opened, copied, imported
//! or written, and only counts and totals are printed. The test returns at once when
//! `FMV_FF7_AUDIO_DATA` is not set. (`FMV_FF7_DATA` is optional: the field and battle folder of an
//! install that keeps its audio elsewhere; it is only scanned, to see that the full scan and the bare
//! discovery agree about it.)

use std::fs;
use std::path::{Path, PathBuf};

use fmv_import::scan::{discover_audio, scan_pc, AudioKind};

fn folder_from_env(name: &str) -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os(name)?);
    path.is_dir().then_some(path)
}

/// The folder that holds `data/`: the scan's way in.
fn install_of(data: &Path) -> &Path {
    data.parent().expect("a data folder sits in an install")
}

#[test]
#[ignore = "reads the owner's real audio files: set FMV_FF7_AUDIO_DATA (and FMV_FF7_DATA)"]
fn the_real_audio_folder_holds_what_an_import_expects() {
    let Some(audio_data) = folder_from_env("FMV_FF7_AUDIO_DATA") else {
        return;
    };
    // the bare discovery, on the data/ folder itself
    let found = discover_audio(&audio_data);
    let summary = found.summary();
    eprintln!(
        "audio: {} ogg, {} wav, music.idx {}, audio.dat {}, audio.fmt {}, {} bytes, {} skipped",
        summary.ogg_files,
        summary.wav_files,
        summary.music_idx,
        summary.audio_dat,
        summary.audio_fmt,
        summary.total_bytes,
        summary.skipped_files
    );
    assert_eq!(summary.ogg_files, 94);
    assert_eq!(summary.wav_files, 4);
    assert!(summary.music_idx && summary.audio_dat && summary.audio_fmt);
    assert_eq!(found.files.len(), 94 + 4 + 3);
    assert!(
        found.warnings.is_empty() && found.refused_links.is_empty() && summary.skipped_files == 0,
        "{:?}",
        found.warnings
    );
    assert_eq!(found.of_kind(AudioKind::MusicOgg).count(), 94);
    // the library paths are lower case and exactly those of the layout
    for f in &found.files {
        assert!(
            f.logical_path == f.logical_path.to_ascii_lowercase(),
            "{}",
            f.logical_path
        );
        assert!(
            f.logical_path.starts_with("data/music_ogg/")
                || f.logical_path.starts_with("data/music/")
                || f.logical_path.starts_with("data/sound/"),
            "{}",
            f.logical_path
        );
        assert!(f.size > 0);
    }
    // sizes are what the file system says
    let on_disk: u64 = found
        .files
        .iter()
        .map(|f| {
            fs::metadata(install_of(&audio_data).join(&f.relative_path))
                .unwrap()
                .len()
        })
        .sum();
    assert_eq!(summary.total_bytes, on_disk);

    // the full scan of the install that holds this data/ folder reports the same
    let report = scan_pc(install_of(&audio_data)).unwrap();
    assert_eq!(report.audio, summary);
    assert_eq!(
        report
            .would_import
            .iter()
            .filter(|w| w.role == "audio")
            .count(),
        94 + 4 + 3
    );

    // the field and battle folder, when given: scan and bare discovery agree about it, whatever it holds
    if let Some(field_data) = folder_from_env("FMV_FF7_DATA") {
        let report = scan_pc(install_of(&field_data)).unwrap();
        assert!(
            report.m1.ready,
            "FMV_FF7_DATA should hold field/flevel.lgp and char.lgp"
        );
        assert_eq!(report.audio, discover_audio(&field_data).summary());
    }
}
