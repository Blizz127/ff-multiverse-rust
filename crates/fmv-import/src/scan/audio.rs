//! The audio files of an FF7 PC install: the music and sound effects the client plays.
//!
//! They are found below the game's `data/` directory, every folder and file name matched
//! ASCII-case-insensitively like everything else in the scan, and kept in the library under
//! lower-case logical paths of exactly this form:
//!
//! - `data/music_ogg/<name>.ogg`: the music tracks of the Steam Edition;
//! - `data/music/<name>.wav` and `data/music/music.idx`: its wav tracks and the music index;
//! - `data/sound/audio.dat` and `data/sound/audio.fmt`: the sound effects and their table.
//!
//! Audio is language-neutral (the same in every language), so it belongs to every import (role
//! `audio`, no language). The scan only lists directories and reads file metadata: it never opens
//! an audio file. What the importer takes, and why:
//!
//! - **Only regular, non-empty files with plain names.** A name is plain when it is at most
//!   [`MAX_AUDIO_NAME`] bytes of `A-Z a-z 0-9 _ . -`, does not start with a dot (that leaves out
//!   the `._name.ogg` files a Mac leaves beside a copied tree), and has something before its
//!   extension. Anything else is left out and said so in a warning, never imported under a
//!   cleaned-up name.
//! - **A bounded number.** At most [`MAX_AUDIO_FILES`] files in all, the three index files first
//!   (without them no track can be found), then the tracks in logical-path order; the rest is
//!   reported in a warning. At most [`MAX_DIR_ENTRIES`] entries of one folder are looked at, so a
//!   crafted folder cannot make the scan do unbounded work.
//! - **One file per library path.** `Bat.ogg` and `bat.ogg` side by side are one logical path:
//!   the first by name is taken, the other is left out and reported.
//! - **Links stay inside the tree.** A link that leads out of the scanned directory is refused by
//!   the very check every other candidate of the scan goes through, and listed in
//!   `refused_links`; a link that stays inside is followed.
//! - **Audio never speaks for the set.** Every warning about audio starts with
//!   [`AUDIO_WARNING_PREFIX`]; `import-pc` keeps such warnings, and the audio files themselves,
//!   out of the identification record of the set. Adding audio to a set by importing the install
//!   again must not change what the set says about itself.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::{candidate_size, find_entry, is_dir, RefusedLink};

/// Start of every warning about audio files (see the module docs).
pub const AUDIO_WARNING_PREFIX: &str = "audio: ";

/// Most audio files one scan lists and one import takes, in all.
pub const MAX_AUDIO_FILES: usize = 512;

/// Longest audio file name taken, in bytes.
pub const MAX_AUDIO_NAME: usize = 64;

/// Most entries of one audio folder the scan looks at (a real one holds under a hundred).
pub const MAX_DIR_ENTRIES: usize = 4096;

/// Most names a warning lists.
const LISTED: usize = 8;

const MUSIC_OGG_DIR: &str = "music_ogg";
const MUSIC_DIR: &str = "music";
const SOUND_DIR: &str = "sound";

/// The kinds of audio file an import takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioKind {
    /// `data/music_ogg/<name>.ogg`
    MusicOgg,
    /// `data/music/<name>.wav`
    MusicWav,
    /// `data/music/music.idx`
    MusicIdx,
    /// `data/sound/audio.dat`
    SoundDat,
    /// `data/sound/audio.fmt`
    SoundFmt,
}

/// The single files: kind, folder and file name (lower case, as in the library).
const INDEX_FILES: [(AudioKind, &str, &str); 3] = [
    (AudioKind::MusicIdx, MUSIC_DIR, "music.idx"),
    (AudioKind::SoundDat, SOUND_DIR, "audio.dat"),
    (AudioKind::SoundFmt, SOUND_DIR, "audio.fmt"),
];

/// The folders that hold one file per track: kind, folder and extension (lower case).
const TRACK_FOLDERS: [(AudioKind, &str, &str); 2] = [
    (AudioKind::MusicOgg, MUSIC_OGG_DIR, "ogg"),
    (AudioKind::MusicWav, MUSIC_DIR, "wav"),
];

/// One audio file an import takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioFile {
    pub kind: AudioKind,
    /// Relative to the scanned directory, with the case found on disk.
    pub relative_path: String,
    /// Where it lands in a library set: lower case, relative to the game directory
    /// (`data/music_ogg/bat.ogg`).
    pub logical_path: String,
    pub size: u64,
}

/// What [`discover_audio`] found.
#[derive(Debug, Clone, Default)]
pub struct AudioDiscovery {
    /// The files an import takes, in logical-path order (see the module docs for the limits).
    pub files: Vec<AudioFile>,
    /// Audio files that are symbolic links leading out of the scanned tree (not followed).
    pub refused_links: Vec<RefusedLink>,
    /// Warnings about what was left out, each starting with [`AUDIO_WARNING_PREFIX`]. Empty when
    /// there is nothing to say (an install without audio says nothing).
    pub warnings: Vec<String>,
    /// How many audio files were looked at and left out: names that are not plain, empty files,
    /// names that differ from another one only in letter case, files over the limit, and the
    /// refused links.
    pub skipped: u64,
}

/// The audio part of the `scan-pc` report.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct AudioSummary {
    /// `.ogg` tracks an import takes (`data/music_ogg`).
    pub ogg_files: u64,
    /// `.wav` tracks an import takes (`data/music`).
    pub wav_files: u64,
    /// `data/music/music.idx` was found.
    pub music_idx: bool,
    /// `data/sound/audio.dat` was found.
    pub audio_dat: bool,
    /// `data/sound/audio.fmt` was found.
    pub audio_fmt: bool,
    /// Total size of the audio files an import takes.
    pub total_bytes: u64,
    /// Audio files that were looked at and left out (see [`AudioDiscovery::skipped`]).
    pub skipped_files: u64,
}

impl AudioDiscovery {
    /// The files of one kind, in logical-path order.
    pub fn of_kind(&self, kind: AudioKind) -> impl Iterator<Item = &AudioFile> {
        self.files.iter().filter(move |f| f.kind == kind)
    }

    /// The counts the `scan-pc` report carries.
    pub fn summary(&self) -> AudioSummary {
        let count = |kind| self.of_kind(kind).count() as u64;
        let has = |kind| self.of_kind(kind).next().is_some();
        AudioSummary {
            ogg_files: count(AudioKind::MusicOgg),
            wav_files: count(AudioKind::MusicWav),
            music_idx: has(AudioKind::MusicIdx),
            audio_dat: has(AudioKind::SoundDat),
            audio_fmt: has(AudioKind::SoundFmt),
            total_bytes: self
                .files
                .iter()
                .fold(0u64, |a, f| a.saturating_add(f.size)),
            skipped_files: self.skipped,
        }
    }
}

/// The audio files below `data_dir`, a game's `data/` directory, usable without a scan around
/// it. Reads names and metadata only. A symbolic link must stay inside `data_dir`; the
/// relative paths of the result start with `data/`.
pub fn discover_audio(data_dir: &Path) -> AudioDiscovery {
    let canon = fs::canonicalize(data_dir).unwrap_or_else(|_| data_dir.to_path_buf());
    discover_audio_in(data_dir, "data", &canon)
}

/// [`discover_audio`] for a `data/` directory inside a scanned tree: `shown_data` is how the
/// directory is spelled relative to the scanned directory (`data`, `ff7/workingdir/data`), and
/// `canon_root` the scanned directory (links must stay inside it).
pub(super) fn discover_audio_in(
    data_dir: &Path,
    shown_data: &str,
    canon_root: &Path,
) -> AudioDiscovery {
    let mut refused: Vec<RefusedLink> = Vec::new();
    let mut odd: Vec<String> = Vec::new();
    let mut empty: Vec<String> = Vec::new();
    let mut cut_folders: Vec<String> = Vec::new();

    // the three index files: tiny, and without them no track can be found
    let mut files: Vec<AudioFile> = Vec::new();
    for (kind, dir, file) in INDEX_FILES {
        let Some((dir_actual, dir_path)) = folder(data_dir, dir) else {
            continue;
        };
        let Some((name, path)) = find_entry(&dir_path, file) else {
            continue;
        };
        let shown = format!("{shown_data}/{dir_actual}/{name}");
        match candidate_size(&path, &shown, canon_root, &mut refused) {
            Some(0) => empty.push(name),
            Some(size) => files.push(AudioFile {
                kind,
                relative_path: shown,
                logical_path: format!("data/{dir}/{file}"),
                size,
            }),
            // not a regular file, a broken link, or refused (and listed)
            None => {}
        }
    }
    let index_count = files.len();

    // the tracks, folder by folder
    let mut tracks: Vec<AudioFile> = Vec::new();
    for (kind, dir, ext) in TRACK_FOLDERS {
        let Some((dir_actual, dir_path)) = folder(data_dir, dir) else {
            continue;
        };
        let Ok(entries) = fs::read_dir(&dir_path) else {
            continue;
        };
        let mut named: Vec<(String, PathBuf)> = Vec::new();
        for (seen, ent) in entries.flatten().enumerate() {
            if seen >= MAX_DIR_ENTRIES {
                cut_folders.push(format!("{shown_data}/{dir_actual}"));
                break;
            }
            let name = ent.file_name().to_string_lossy().into_owned();
            if stem_of(&name, ext).is_some() {
                named.push((name, ent.path()));
            }
        }
        // a fixed order, so that what is left out by a limit never depends on the directory's
        named.sort_by(|a, b| {
            a.0.to_ascii_lowercase()
                .cmp(&b.0.to_ascii_lowercase())
                .then_with(|| a.0.cmp(&b.0))
        });
        for (name, path) in named {
            if !is_plain_name(&name) {
                odd.push(name);
                continue;
            }
            let shown = format!("{shown_data}/{dir_actual}/{name}");
            match candidate_size(&path, &shown, canon_root, &mut refused) {
                Some(0) => empty.push(name),
                Some(size) => tracks.push(AudioFile {
                    kind,
                    relative_path: shown,
                    logical_path: format!("data/{dir}/{}", name.to_ascii_lowercase()),
                    size,
                }),
                None => {}
            }
        }
    }

    // one file per library path: of `Bat.ogg` and `bat.ogg` the first by name is taken
    tracks.sort_by(|a, b| {
        a.logical_path
            .cmp(&b.logical_path)
            .then_with(|| a.relative_path.cmp(&b.relative_path))
    });
    let mut clashes: Vec<String> = Vec::new();
    let mut previous: Option<String> = None;
    tracks.retain(|t| {
        if previous.as_deref() == Some(t.logical_path.as_str()) {
            clashes.push(t.relative_path.clone());
            return false;
        }
        previous = Some(t.logical_path.clone());
        true
    });
    // the limit: the index files are already in, the tracks fill what is left
    let room = MAX_AUDIO_FILES.saturating_sub(index_count);
    let over = tracks.len().saturating_sub(room);
    tracks.truncate(room);
    files.extend(tracks);
    files.sort_by(|a, b| a.logical_path.cmp(&b.logical_path));

    for link in &mut refused {
        link.audio = true;
    }
    refused.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    refused.dedup();

    let mut warnings = Vec::new();
    if !odd.is_empty() {
        warnings.push(format!(
            "{AUDIO_WARNING_PREFIX}{} file(s) were left out because their names are not plain (only letters, digits, '_', '.' and '-' are taken, at most {MAX_AUDIO_NAME} bytes, no leading dot): {}",
            odd.len(),
            listed_debug(&odd)
        ));
    }
    if !empty.is_empty() {
        warnings.push(format!(
            "{AUDIO_WARNING_PREFIX}{} empty (0-byte) file(s) were left out: {}",
            empty.len(),
            listed(&empty)
        ));
    }
    if !clashes.is_empty() {
        warnings.push(format!(
            "{AUDIO_WARNING_PREFIX}{} file(s) differ from another audio file only in letter case, which is one library path: the first by name was taken, these were left out: {}",
            clashes.len(),
            listed(&clashes)
        ));
    }
    if over > 0 {
        warnings.push(format!(
            "{AUDIO_WARNING_PREFIX}an import takes at most {MAX_AUDIO_FILES} audio files (the index files first, then the tracks in library-path order); {over} more were left out"
        ));
    }
    if !cut_folders.is_empty() {
        warnings.push(format!(
            "{AUDIO_WARNING_PREFIX}more than {MAX_DIR_ENTRIES} entries in {}: only the first {MAX_DIR_ENTRIES} of each were looked at",
            cut_folders.join(", ")
        ));
    }
    if !refused.is_empty() {
        let names: Vec<String> = refused.iter().map(|l| l.relative_path.clone()).collect();
        warnings.push(format!(
            "{AUDIO_WARNING_PREFIX}{} file(s) are symbolic links that lead outside the scanned directory and were left out: {}. Links that stay inside the directory are followed; for these, scan the directory that really holds the files, or copy them into the install.",
            refused.len(),
            listed(&names)
        ));
    }
    let skipped = [odd.len(), empty.len(), clashes.len(), over, refused.len()]
        .iter()
        .fold(0u64, |a, n| a.saturating_add(*n as u64));
    AudioDiscovery {
        files,
        refused_links: refused,
        warnings,
        skipped,
    }
}

/// The folder `name` (any letter case) of `data_dir`, with the name it has on disk.
fn folder(data_dir: &Path, name: &str) -> Option<(String, PathBuf)> {
    find_entry(data_dir, name).filter(|(_, p)| is_dir(p))
}

/// What comes before the extension `ext` (lower case, without the dot) of `name`, in any letter
/// case: `Some("bat")` for `BAT.ogg`. `None` when the extension is another one, or nothing comes
/// before it.
fn stem_of<'a>(name: &'a str, ext: &str) -> Option<&'a str> {
    let dot = name.len().checked_sub(ext.len().checked_add(1)?)?;
    let stem = name.get(..dot).filter(|s| !s.is_empty())?;
    let tail = name.get(dot..)?.strip_prefix('.')?;
    tail.eq_ignore_ascii_case(ext).then_some(stem)
}

/// A name the importer takes as it is: letters, digits, `_`, `.` and `-` only, at most
/// [`MAX_AUDIO_NAME`] bytes, not starting with a dot.
fn is_plain_name(name: &str) -> bool {
    !name.starts_with('.')
        && name.len() <= MAX_AUDIO_NAME
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// At most [`LISTED`] names, comma separated, with how many more there are.
fn listed(names: &[String]) -> String {
    let mut out = names
        .iter()
        .take(LISTED)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    if names.len() > LISTED {
        out.push_str(&format!(" and {} more", names.len().saturating_sub(LISTED)));
    }
    out
}

/// [`listed`] for names that may hold anything: quoted, escaped and cut short.
fn listed_debug(names: &[String]) -> String {
    let quoted: Vec<String> = names
        .iter()
        .take(LISTED)
        .map(|n| format!("{:?}", n.chars().take(MAX_AUDIO_NAME).collect::<String>()))
        .collect();
    let mut out = quoted.join(", ");
    if names.len() > LISTED {
        out.push_str(&format!(" and {} more", names.len().saturating_sub(LISTED)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::scan_pc;
    use crate::synthetic::{steam_edition_library, write_file};

    fn put(root: &Path, rel: &str, bytes: &[u8]) {
        write_file(root, rel, bytes);
    }

    fn logical(d: &AudioDiscovery) -> Vec<&str> {
        d.files.iter().map(|f| f.logical_path.as_str()).collect()
    }

    fn relative(d: &AudioDiscovery) -> Vec<&str> {
        d.files.iter().map(|f| f.relative_path.as_str()).collect()
    }

    /// A small install around the audio: the field archive an import needs (its data folder is
    /// spelled `data`, the one folder of that name: a case-sensitive file system would take `Data`
    /// for another), plus `extra`.
    fn install_with(root: &Path, data: &str, extra: &[&str]) {
        put(root, "ff7.exe", b"exe bytes");
        put(root, &format!("{data}/field/flevel.lgp"), b"field bytes");
        put(root, &format!("{data}/field/char.lgp"), b"char bytes");
        for rel in extra {
            put(root, rel, format!("synthetic audio {rel}").as_bytes());
        }
    }

    const ALL_FIVE: [&str; 6] = [
        "data/music_ogg/bat.ogg",
        "data/music_ogg/victory.ogg",
        "data/music/heart.wav",
        "data/music/music.idx",
        "data/sound/audio.dat",
        "data/sound/audio.fmt",
    ];

    // -----------------------------------------------------------------------------------------
    // discovery on a bare data directory
    // -----------------------------------------------------------------------------------------

    #[test]
    fn the_five_kinds_are_found_in_any_letter_case_and_stored_in_lower_case() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        put(&data, "MUSIC_OGG/BAT.OGG", b"ogg one");
        put(&data, "MUSIC_OGG/Victory.Ogg", b"ogg number two");
        put(&data, "Music/Heart.WAV", b"wav");
        put(&data, "Music/MUSIC.IDX", b"idx!");
        put(&data, "SOUND/Audio.Dat", b"sfx data");
        put(&data, "SOUND/AUDIO.FMT", b"fmt");
        let d = discover_audio(&data);
        assert_eq!(
            logical(&d),
            [
                "data/music/heart.wav",
                "data/music/music.idx",
                "data/music_ogg/bat.ogg",
                "data/music_ogg/victory.ogg",
                "data/sound/audio.dat",
                "data/sound/audio.fmt",
            ]
        );
        // the paths to read keep the case found on disk
        assert_eq!(
            relative(&d),
            [
                "data/Music/Heart.WAV",
                "data/Music/MUSIC.IDX",
                "data/MUSIC_OGG/BAT.OGG",
                "data/MUSIC_OGG/Victory.Ogg",
                "data/SOUND/Audio.Dat",
                "data/SOUND/AUDIO.FMT",
            ]
        );
        assert!(d.warnings.is_empty() && d.refused_links.is_empty() && d.skipped == 0);
        assert_eq!(d.of_kind(AudioKind::MusicOgg).count(), 2);
        assert_eq!(
            d.summary(),
            AudioSummary {
                ogg_files: 2,
                wav_files: 1,
                music_idx: true,
                audio_dat: true,
                audio_fmt: true,
                total_bytes: 7 + 14 + 3 + 4 + 8 + 3,
                skipped_files: 0,
            }
        );
    }

    #[test]
    fn a_tree_without_audio_has_nothing_to_say() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        put(&data, "field/flevel.lgp", b"field");
        // a FILE called music, an empty folder called sound, and audio-looking files elsewhere
        put(&data, "music", b"not a folder");
        fs::create_dir_all(data.join("sound")).unwrap();
        put(&data, "field/bat.ogg", b"not in a music folder");
        let d = discover_audio(&data);
        assert!(d.files.is_empty() && d.warnings.is_empty() && d.refused_links.is_empty());
        assert_eq!(d.skipped, 0);
        assert_eq!(d.summary(), AudioSummary::default());
        // and a directory that is not there at all
        let none = discover_audio(&tmp.path().join("nowhere"));
        assert!(none.files.is_empty() && none.warnings.is_empty());
    }

    #[test]
    fn odd_names_and_empty_files_are_left_out_and_said_and_other_files_are_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        let name_64 = format!("{}.ogg", "b".repeat(60));
        let name_65 = format!("{}.ogg", "a".repeat(61));
        assert_eq!((name_64.len(), name_65.len()), (64, 65));
        for ok in ["ok.ogg", name_64.as_str()] {
            put(&data, &format!("music_ogg/{ok}"), b"track");
        }
        for odd in [
            "a b.ogg",
            "\u{e9}.ogg",
            ".hidden.ogg",
            "._ok.ogg",
            "semi;colon.ogg",
            name_65.as_str(),
        ] {
            put(&data, &format!("music_ogg/{odd}"), b"track");
        }
        put(&data, "music_ogg/zero.ogg", b"");
        put(&data, "music/music.idx", b"");
        put(&data, "music/clip.wav", b"wav");
        // not audio at all: ignored without a word
        put(&data, "music_ogg/.ogg", b"no name before the extension");
        put(&data, "music_ogg/readme.txt", b"text");
        put(&data, "music_ogg/x.wav", b"a wav in the ogg folder");
        put(&data, "music/clip.ogg", b"an ogg in the wav folder");
        fs::create_dir_all(data.join("music_ogg/folder.ogg")).unwrap();

        let d = discover_audio(&data);
        assert_eq!(
            logical(&d),
            [
                "data/music/clip.wav".to_string(),
                format!("data/music_ogg/{name_64}"),
                "data/music_ogg/ok.ogg".to_string(),
            ]
        );
        assert_eq!(d.skipped, 6 + 2, "six odd names and two empty files");
        assert_eq!(d.warnings.len(), 2, "{:?}", d.warnings);
        assert!(d
            .warnings
            .iter()
            .all(|w| w.starts_with(AUDIO_WARNING_PREFIX)));
        let odd = &d.warnings[0];
        assert!(
            odd.contains("6 file(s)") && odd.contains("not plain") && odd.contains("\"a b.ogg\""),
            "{odd}"
        );
        let empty = &d.warnings[1];
        assert!(
            empty.contains("2 empty") && empty.contains("zero.ogg") && empty.contains("music.idx"),
            "{empty}"
        );
        // the index file that was empty is simply not there
        assert!(!d.summary().music_idx);
    }

    #[test]
    fn two_spellings_of_one_name_are_one_library_path_and_the_first_by_name_is_taken() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        put(&data, "music_ogg/bat.ogg", b"lower");
        put(&data, "music_ogg/Bat.ogg", b"upper");
        if fs::read_dir(data.join("music_ogg")).unwrap().count() != 2 {
            return; // a case-insensitive file system cannot hold both
        }
        let d = discover_audio(&data);
        assert_eq!(
            relative(&d),
            ["data/music_ogg/Bat.ogg"],
            "upper case sorts first"
        );
        assert_eq!(d.skipped, 1);
        assert_eq!(d.warnings.len(), 1, "{:?}", d.warnings);
        assert!(
            d.warnings[0].contains("letter case") && d.warnings[0].contains("bat.ogg"),
            "{:?}",
            d.warnings
        );
    }

    #[test]
    fn the_limit_keeps_the_index_files_first_then_the_tracks_in_name_order() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        for i in 0..515 {
            put(&data, &format!("music_ogg/t{i:03}.ogg"), b"t");
        }
        for rel in [
            "music/w0.wav",
            "music/w1.wav",
            "music/music.idx",
            "sound/audio.dat",
            "sound/audio.fmt",
        ] {
            put(&data, rel, b"x");
        }
        let d = discover_audio(&data);
        assert_eq!(d.files.len(), MAX_AUDIO_FILES);
        let s = d.summary();
        assert!(
            s.music_idx && s.audio_dat && s.audio_fmt,
            "the index files always survive: {s:?}"
        );
        assert_eq!(s.wav_files, 2);
        // 512 - 3 index files = 509 tracks: the two wavs (data/music sorts before data/music_ogg)
        // and the first 507 oggs by name; the last eight are left out
        assert_eq!(s.ogg_files, 507);
        let names = logical(&d);
        assert!(names.contains(&"data/music_ogg/t506.ogg"));
        assert!(!names.contains(&"data/music_ogg/t507.ogg"));
        assert_eq!(d.skipped, 8);
        assert_eq!(d.warnings.len(), 1, "{:?}", d.warnings);
        assert!(
            d.warnings[0].starts_with(AUDIO_WARNING_PREFIX)
                && d.warnings[0].contains("at most 512")
                && d.warnings[0].contains("8 more"),
            "{:?}",
            d.warnings
        );
        // the files are listed in logical-path order, whatever the directory order was
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }

    #[test]
    fn a_folder_with_more_entries_than_the_bound_is_cut_and_says_so() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        fs::create_dir_all(data.join("music_ogg")).unwrap();
        for i in 0..MAX_DIR_ENTRIES + 10 {
            fs::write(data.join(format!("music_ogg/x{i:05}.ogg")), b"t").unwrap();
        }
        let d = discover_audio(&data);
        assert_eq!(d.files.len(), MAX_AUDIO_FILES);
        assert!(
            d.warnings
                .iter()
                .any(|w| w.starts_with(AUDIO_WARNING_PREFIX)
                    && w.contains("entries in data/music_ogg")),
            "{:?}",
            d.warnings
        );
        assert!(
            d.warnings.iter().any(|w| w.contains("at most 512")),
            "{:?}",
            d.warnings
        );
    }

    #[cfg(unix)]
    #[test]
    fn links_out_of_the_tree_are_refused_and_listed_and_links_inside_are_followed() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("install/data");
        let outside = tmp.path().join("elsewhere");
        put(&outside, "secret.ogg", b"NOT-A-REAL-SECRET");
        put(&outside, "audio.dat", b"NOT-A-REAL-SECRET either");
        put(&data, "music_ogg/real.ogg", b"real");
        put(&data, "stash/inner.ogg", b"inner");
        symlink(outside.join("secret.ogg"), data.join("music_ogg/leak.ogg")).unwrap();
        symlink(
            data.join("stash/inner.ogg"),
            data.join("music_ogg/inner.ogg"),
        )
        .unwrap();
        // a whole folder that is a link out of the tree takes the file behind it with it
        symlink(&outside, data.join("sound")).unwrap();
        // a link that leads nowhere is just not a file
        symlink(tmp.path().join("gone"), data.join("music_ogg/broken.ogg")).unwrap();

        let d = discover_audio(&data);
        assert_eq!(
            logical(&d),
            ["data/music_ogg/inner.ogg", "data/music_ogg/real.ogg"]
        );
        let refused: Vec<&str> = d
            .refused_links
            .iter()
            .map(|l| l.relative_path.as_str())
            .collect();
        assert_eq!(refused, ["data/music_ogg/leak.ogg", "data/sound/audio.dat"]);
        assert!(d
            .refused_links
            .iter()
            .all(|l| l.audio && l.reason.contains("outside")));
        assert_eq!(d.skipped, 2);
        assert_eq!(d.warnings.len(), 1, "{:?}", d.warnings);
        assert!(
            d.warnings[0].starts_with(AUDIO_WARNING_PREFIX)
                && d.warnings[0].contains("leak.ogg")
                && d.warnings[0].contains("audio.dat"),
            "{:?}",
            d.warnings
        );
        // no local path of where a link leads
        let shown = format!("{:?}{:?}", d.warnings, d.refused_links);
        assert!(!shown.contains(&outside.display().to_string()), "{shown}");
        assert_eq!(
            fs::read(outside.join("secret.ogg")).unwrap(),
            b"NOT-A-REAL-SECRET"
        );
    }

    // -----------------------------------------------------------------------------------------
    // inside a scan
    // -----------------------------------------------------------------------------------------

    #[test]
    fn a_scan_lists_audio_as_language_neutral_files_every_import_takes() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("FF7");
        install_with(
            &root,
            "Data",
            &[
                "Data/Music_Ogg/BAT.ogg",
                "Data/Music_Ogg/Victory.ogg",
                "Data/Music/Heart.wav",
                "Data/Music/MUSIC.IDX",
                "Data/Sound/audio.dat",
                "Data/Sound/audio.fmt",
            ],
        );
        let r = scan_pc(&root).unwrap();
        let audio: Vec<_> = r
            .would_import
            .iter()
            .filter(|w| w.role == "audio")
            .collect();
        assert_eq!(
            audio
                .iter()
                .map(|w| w.logical_path.as_str())
                .collect::<Vec<_>>(),
            [
                "data/music/heart.wav",
                "data/music/music.idx",
                "data/music_ogg/bat.ogg",
                "data/music_ogg/victory.ogg",
                "data/sound/audio.dat",
                "data/sound/audio.fmt",
            ]
        );
        assert_eq!(
            audio
                .iter()
                .map(|w| w.source_relative_path.as_str())
                .collect::<Vec<_>>(),
            [
                "Data/Music/Heart.wav",
                "Data/Music/MUSIC.IDX",
                "Data/Music_Ogg/BAT.ogg",
                "Data/Music_Ogg/Victory.ogg",
                "Data/Sound/audio.dat",
                "Data/Sound/audio.fmt",
            ]
        );
        assert!(audio.iter().all(|w| w.language.is_none()
            && !w.language_inferred
            && w.variant_of.is_none()
            && w.in_default_import));
        let audio_bytes: u64 = audio.iter().map(|w| w.size).sum();
        assert_eq!(r.audio.total_bytes, audio_bytes);
        assert_eq!(
            (
                r.audio.ogg_files,
                r.audio.wav_files,
                r.audio.music_idx,
                r.audio.audio_dat,
                r.audio.audio_fmt
            ),
            (2, 1, true, true, true)
        );
        // the totals count audio like any other candidate
        assert_eq!(r.default_import_bytes, r.would_import_bytes);
        assert!(r.would_import_bytes > audio_bytes);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        assert!(
            r.notes.iter().any(|n| n.contains("import-pc again")),
            "{:?}",
            r.notes
        );
        // the audio is not a required file and changes no milestone
        assert!(r.required_files.iter().all(|f| f.name != "music.idx"));
        assert!(r.m1.ready && !r.m2.ready);
    }

    /// New keys come at the end of the report and nothing else about it moves.
    #[test]
    fn the_audio_summary_is_the_last_key_and_the_existing_ones_are_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let plain = tmp.path().join("plain");
        let with_audio = tmp.path().join("with-audio");
        install_with(&plain, "data", &[]);
        install_with(&with_audio, "data", &ALL_FIVE);
        let (a, b) = (scan_pc(&plain).unwrap(), scan_pc(&with_audio).unwrap());

        // an install without audio: the key is there, all zero, and nothing else says a word
        assert_eq!(a.audio, AudioSummary::default());
        assert!(a.warnings.is_empty());
        assert!(!a.notes.iter().any(|n| n.contains("audio")));

        for report in [&a, &b] {
            let text = serde_json::to_string(report).unwrap();
            let summary = serde_json::to_string(&report.audio).unwrap();
            assert!(
                text.ends_with(&format!(",\"audio\":{summary}}}")),
                "audio is the last key: ...{}",
                text.get(text.len().saturating_sub(300)..).unwrap_or(&text)
            );
            assert!(text.find("\"notes\":").unwrap() < text.find("\"audio\":").unwrap());
        }
        let summary = serde_json::to_value(&a.audio).unwrap();
        assert_eq!(
            summary,
            serde_json::json!({
                "ogg_files": 0, "wav_files": 0, "music_idx": false, "audio_dat": false,
                "audio_fmt": false, "total_bytes": 0, "skipped_files": 0
            })
        );

        // every key but the ones audio is about holds the same value with and without audio
        let (va, vb) = (
            serde_json::to_value(&a).unwrap(),
            serde_json::to_value(&b).unwrap(),
        );
        let (oa, ob) = (va.as_object().unwrap(), vb.as_object().unwrap());
        assert_eq!(oa.keys().collect::<Vec<_>>(), ob.keys().collect::<Vec<_>>());
        for (key, value) in oa {
            if [
                "root",
                "would_import",
                "would_import_bytes",
                "default_import_bytes",
                "notes",
                "audio",
            ]
            .contains(&key.as_str())
            {
                continue;
            }
            assert_eq!(value, &ob[key], "{key}");
        }
        // the entries that were there are unchanged; the new ones are all audio
        let without: Vec<_> = b
            .would_import
            .iter()
            .filter(|w| w.role != "audio")
            .collect();
        assert_eq!(without.len(), a.would_import.len());
        for (x, y) in a.would_import.iter().zip(without) {
            assert_eq!(
                serde_json::to_value(x).unwrap(),
                serde_json::to_value(y).unwrap()
            );
        }
    }

    #[test]
    fn audio_is_found_below_the_steam_editions_game_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let install = steam_edition_library(tmp.path(), true);
        for rel in ALL_FIVE {
            put(&install.join("ff7/workingdir"), rel, b"synthetic audio");
        }
        let r = scan_pc(&install).unwrap();
        let audio: Vec<_> = r
            .would_import
            .iter()
            .filter(|w| w.role == "audio")
            .collect();
        assert_eq!(audio.len(), 6);
        // library paths are relative to the game directory, read paths to the scanned one
        assert!(audio
            .iter()
            .any(|w| w.logical_path == "data/music_ogg/bat.ogg"
                && w.source_relative_path == "ff7/workingdir/data/music_ogg/bat.ogg"));
        // five languages, the default one is English: audio goes with it, like the other neutral files
        assert!(audio.iter().all(|w| w.in_default_import));
        assert_eq!(r.audio.ogg_files, 2);
    }

    #[test]
    fn audio_follows_the_rule_for_the_other_language_neutral_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("steam");
        for f in [
            "ff7_en.exe",
            "ff7_fr.exe",
            "data/lang-en/field/flevel.lgp",
            "data/lang-fr/field/flevel.lgp",
            "data/sound/audio.dat",
        ] {
            put(&root, f, b"synthetic");
        }
        let r = scan_pc(&root).unwrap();
        // no language is implied, so there is no default import: nothing is flagged, audio included
        assert!(r.language_selection_required);
        assert!(r.would_import.iter().all(|w| !w.in_default_import));
        // a chosen language takes the neutral files, and audio is one of them
        let sel = r.select(Some("fr")).unwrap();
        let taken: Vec<&str> = sel.files.iter().map(|w| w.logical_path.as_str()).collect();
        assert!(
            taken.contains(&"data/sound/audio.dat")
                && taken.contains(&"data/lang-fr/field/flevel.lgp"),
            "{taken:?}"
        );
        assert!(!taken.iter().any(|p| p.contains("lang-en")), "{taken:?}");
    }

    #[cfg(unix)]
    #[test]
    fn a_refused_audio_link_is_listed_with_the_others_and_has_a_warning_of_its_own() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("FF7");
        let outside = tmp.path().join("elsewhere");
        install_with(
            &root,
            "data",
            &["data/music_ogg/real.ogg", "data/battle/scene.bin"],
        );
        put(&outside, "secret.bin", b"NOT-A-REAL-SECRET");
        symlink(
            outside.join("secret.bin"),
            root.join("data/music_ogg/leak.ogg"),
        )
        .unwrap();

        // only an audio link: listed, and the warning about the game files stays silent
        let r = scan_pc(&root).unwrap();
        assert_eq!(r.refused_links.len(), 1, "{:?}", r.refused_links);
        assert_eq!(r.refused_links[0].relative_path, "data/music_ogg/leak.ogg");
        assert!(r.refused_links[0].audio);
        assert!(!r
            .would_import
            .iter()
            .any(|w| w.logical_path.ends_with("leak.ogg")));
        assert_eq!(r.warnings.len(), 1, "{:?}", r.warnings);
        assert!(
            r.warnings[0].starts_with(AUDIO_WARNING_PREFIX)
                && r.warnings[0].contains("symbolic link")
        );
        assert_eq!(r.audio.skipped_files, 1);

        // next to a refused link of a game file: both listed, each in its own warning
        fs::remove_file(root.join("data/battle/scene.bin")).unwrap();
        symlink(
            outside.join("secret.bin"),
            root.join("data/battle/scene.bin"),
        )
        .unwrap();
        let r = scan_pc(&root).unwrap();
        let listed: Vec<&str> = r
            .refused_links
            .iter()
            .map(|l| l.relative_path.as_str())
            .collect();
        assert_eq!(listed, ["data/battle/scene.bin", "data/music_ogg/leak.ogg"]);
        assert_eq!(r.warnings.len(), 2, "{:?}", r.warnings);
        let (game, audio) = (&r.warnings[0], &r.warnings[1]);
        assert!(
            game.contains("scene.bin")
                && !game.contains("leak.ogg")
                && !game.starts_with(AUDIO_WARNING_PREFIX),
            "{game}"
        );
        assert!(
            audio.starts_with(AUDIO_WARNING_PREFIX)
                && audio.contains("leak.ogg")
                && !audio.contains("scene.bin"),
            "{audio}"
        );
        // the flag is not part of the report
        let json = serde_json::to_value(&r).unwrap();
        for link in json["refused_links"].as_array().unwrap() {
            let mut keys: Vec<&String> = link.as_object().unwrap().keys().collect();
            keys.sort();
            assert_eq!(keys, ["reason", "relative_path"]);
        }
        assert!(!json.to_string().contains(&outside.display().to_string()));
    }
}
