//! `import-pc` and audio (music and sound effects), on SYNTHETIC data only: fake install trees
//! with a few made-up bytes per file. Nothing here is, or is derived from, game data; every path
//! is a temp dir.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use fmv_core::SourceRef;
use fmv_library::ff7::{resolve_audio, Ff7Source};
use fmv_library::{hash_file, Library};

use super::tests::{manifest_of, pc_args, snapshot, touch};
use super::*;
use crate::scan::{scan_pc, AUDIO_WARNING_PREFIX, MAX_AUDIO_FILES};
use crate::synthetic::steam_edition_library;

/// The five kinds of audio file: where each sits in the install (the letter cases of the folders
/// are consistent, each name has its own), the library path it lands at, and its bytes.
const AUDIO: [(&str, &str, &[u8]); 6] = [
    (
        "data/Music_Ogg/BAT.ogg",
        "data/music_ogg/bat.ogg",
        b"synthetic ogg track one (not game data)",
    ),
    (
        "data/Music_Ogg/victory.ogg",
        "data/music_ogg/victory.ogg",
        b"synthetic ogg track two",
    ),
    (
        "data/MUSIC/heart.wav",
        "data/music/heart.wav",
        b"synthetic wav track",
    ),
    (
        "data/MUSIC/Music.IDX",
        "data/music/music.idx",
        b"synthetic music index",
    ),
    (
        "data/sound/audio.dat",
        "data/sound/audio.dat",
        b"synthetic sound bank",
    ),
    (
        "data/sound/AUDIO.FMT",
        "data/sound/audio.fmt",
        b"synthetic sound table",
    ),
];

/// What an install holds besides audio: the library paths of its game files.
const GAME: [&str; 4] = [
    "data/battle/scene.bin",
    "data/field/char.lgp",
    "data/field/flevel.lgp",
    "ff7.exe",
];

fn game_files(root: &Path) {
    touch(root, "ff7.exe", b"synthetic exe");
    touch(root, "data/field/flevel.lgp", b"synthetic field archive");
    touch(root, "data/field/char.lgp", b"synthetic character archive");
    touch(root, "data/battle/scene.bin", b"synthetic scene file");
}

fn add_audio(root: &Path) {
    for (rel, _, bytes) in AUDIO {
        touch(root, rel, bytes);
    }
}

fn audio_paths() -> BTreeSet<&'static str> {
    AUDIO.iter().map(|(_, logical, _)| *logical).collect()
}

fn is_audio(logical: &str) -> bool {
    ["data/music_ogg/", "data/music/", "data/sound/"]
        .iter()
        .any(|p| logical.starts_with(p))
}

fn manifest_path(state: &Path, set_id: &str) -> PathBuf {
    state
        .join("library/sets")
        .join(set_id)
        .join("manifest.json")
}

fn library_source(state: &Path) -> Ff7Source {
    Ff7Source::Library {
        state_dir: Some(state.to_path_buf()),
        set: None,
    }
}

#[test]
fn an_install_with_audio_imports_all_five_kinds_under_their_exact_paths_with_their_hashes() {
    let tmp = tempfile::tempdir().unwrap();
    let install = tmp.path().join("FF7");
    game_files(&install);
    add_audio(&install);
    let state = tmp.path().join("state");
    let before = snapshot(&install);

    let r = import_pc(&pc_args(&install, &state), 1).unwrap();
    assert_eq!(snapshot(&install), before, "the install is left as it was");
    let logical: BTreeSet<&str> = r.files.iter().map(|f| f.logical_path.as_str()).collect();
    assert_eq!(logical.len(), GAME.len() + AUDIO.len(), "{logical:?}");
    for want in GAME.iter().chain(audio_paths().iter()) {
        assert!(logical.contains(want), "{want} missing from {logical:?}");
    }
    assert_eq!(r.totals.files, 10);
    assert_eq!(r.totals.entries_added, 10);
    assert!(r
        .files
        .iter()
        .all(|f| f.entry == "added" && f.blob == "created"));
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    // the same checks as for the field archive: stamps, and every file read again and hashed
    assert!(r.source_files_unchanged);
    assert_eq!(r.source_evidence.files_checked, 10);
    assert!(r.source_evidence.stamps_unchanged);
    assert_eq!(r.source_evidence.content_rehash_unchanged, Some(true));

    // the manifest: the exact logical paths, the hashes of the files, the install as their source
    let m = manifest_of(&state, &r.set_id);
    assert_eq!(m.entries.len(), 10);
    let root = fs::canonicalize(&install).unwrap().display().to_string();
    for (rel, want, bytes) in AUDIO {
        let e = m
            .find(want)
            .unwrap_or_else(|| panic!("{want} is not in the manifest"));
        assert_eq!(e.blake3, hash_file(&install.join(rel)).unwrap(), "{want}");
        assert_eq!(e.size, bytes.len() as u64, "{want}");
        assert_eq!(
            e.source,
            SourceRef::Install { path: root.clone() },
            "{want}"
        );
        let listed = r.files.iter().find(|f| f.logical_path == want).unwrap();
        assert_eq!(
            (listed.blake3.as_str(), listed.size),
            (e.blake3.as_str(), e.size)
        );
    }
    // nothing about audio is in what the set says about itself
    assert!(
        r.identification
            .caveats
            .iter()
            .all(|c| !c.contains("audio")),
        "{:?}",
        r.identification.caveats
    );

    // the blobs hold the files, and what the client uses finds them by those paths
    let lib = Library::open_existing(&state).unwrap();
    for (_, want, bytes) in AUDIO {
        assert_eq!(
            fs::read(lib.get(&r.set_id, want).unwrap().unwrap()).unwrap(),
            bytes,
            "{want}"
        );
    }
    let found = resolve_audio(&library_source(&state)).unwrap();
    assert_eq!(
        found.music_ogg.keys().collect::<Vec<_>>(),
        ["bat", "victory"]
    );
    assert_eq!(found.music_wav.keys().collect::<Vec<_>>(), ["heart"]);
    assert_eq!(
        fs::read(found.music_file("BAT").unwrap()).unwrap(),
        AUDIO[0].2
    );
    assert_eq!(
        fs::read(found.music_file("heart").unwrap()).unwrap(),
        AUDIO[2].2
    );
    assert_eq!(
        fs::read(found.music_idx.as_ref().unwrap()).unwrap(),
        AUDIO[3].2
    );
    assert_eq!(
        fs::read(found.sfx_dat.as_ref().unwrap()).unwrap(),
        AUDIO[4].2
    );
    assert_eq!(
        fs::read(found.sfx_fmt.as_ref().unwrap()).unwrap(),
        AUDIO[5].2
    );
}

#[test]
fn an_install_without_audio_imports_exactly_as_before() {
    let tmp = tempfile::tempdir().unwrap();
    let install = tmp.path().join("FF7");
    game_files(&install);
    let report = scan_pc(&install).unwrap();
    assert_eq!(report.audio, crate::scan::AudioSummary::default());
    assert!(report.warnings.is_empty() && report.refused_links.is_empty());

    let state = tmp.path().join("state");
    let r = import_pc(&pc_args(&install, &state), 1).unwrap();
    let logical: Vec<&str> = r.files.iter().map(|f| f.logical_path.as_str()).collect();
    assert_eq!(logical, GAME);
    assert_eq!(r.totals.files, 4);
    assert!(
        r.warnings.is_empty(),
        "no audio, no warning: {:?}",
        r.warnings
    );
    let m = manifest_of(&state, &r.set_id);
    assert!(m.entries.iter().all(|e| !is_audio(&e.logical_path)));
    assert!(resolve_audio(&library_source(&state)).unwrap().is_empty());
}

/// The case this exists for: a library that was imported before audio was a thing gets it by
/// running the import again.
#[test]
fn importing_again_after_audio_was_added_adds_only_the_audio_to_the_same_set() {
    let tmp = tempfile::tempdir().unwrap();
    let install = tmp.path().join("FF7");
    game_files(&install);
    let state = tmp.path().join("state");
    let first = import_pc(&pc_args(&install, &state), 7).unwrap();
    let before = manifest_of(&state, &first.set_id);
    let path = manifest_path(&state, &first.set_id);
    let before_bytes = fs::read(&path).unwrap();
    assert!(resolve_audio(&library_source(&state)).unwrap().is_empty());

    // importing the same install again changes nothing, not even the manifest file
    let same = import_pc(&pc_args(&install, &state), 8).unwrap();
    assert!(same.files.iter().all(|f| f.entry == "unchanged"));
    assert_eq!(fs::read(&path).unwrap(), before_bytes);

    // the audio arrives; the same labels (nothing is given: the scan's) mean the same set. The
    // same `now` too, so that the only thing that may differ is what the audio adds
    add_audio(&install);
    let second = import_pc(&pc_args(&install, &state), 7).unwrap();
    assert_eq!(second.set_id, first.set_id);
    for f in &second.files {
        let want = if is_audio(&f.logical_path) {
            "added"
        } else {
            "unchanged"
        };
        assert_eq!(f.entry, want, "{}", f.logical_path);
    }
    assert_eq!(
        (second.totals.entries_added, second.totals.entries_unchanged),
        (6, 4)
    );
    assert_eq!(
        Library::open_existing(&state).unwrap().list_sets().unwrap(),
        std::slice::from_ref(&first.set_id)
    );

    let after = manifest_of(&state, &first.set_id);
    // everything but the entries is exactly what it was, the identification record included
    assert_eq!(after.provenance, before.provenance);
    assert_eq!(after.identification, before.identification);
    assert_eq!(second.identification, first.identification);
    assert_eq!(
        (
            after.format_version,
            &after.set_id,
            after.imported_at,
            &after.importer_version,
            &after.discs
        ),
        (
            before.format_version,
            &before.set_id,
            before.imported_at,
            &before.importer_version,
            &before.discs
        )
    );
    // every entry that was there is untouched; the new ones are exactly the audio
    for e in &before.entries {
        assert_eq!(after.find(&e.logical_path), Some(e), "{}", e.logical_path);
    }
    let added: BTreeSet<&str> = after
        .entries
        .iter()
        .filter(|e| before.find(&e.logical_path).is_none())
        .map(|e| e.logical_path.as_str())
        .collect();
    assert_eq!(added, audio_paths());
    assert_eq!(after.entries.len(), 10);
    // and the client now finds it
    assert!(!resolve_audio(&library_source(&state)).unwrap().is_empty());

    // a third import of the same install is a no-op again, down to the manifest file
    let bytes = fs::read(&path).unwrap();
    let third = import_pc(&pc_args(&install, &state), 9).unwrap();
    assert!(third.files.iter().all(|f| f.entry == "unchanged"));
    assert_eq!(fs::read(&path).unwrap(), bytes);
}

/// The Steam Edition is the owner's real case: its install root, five languages, the library set
/// that exists, and audio found below `ff7/workingdir/data`.
#[test]
fn the_steam_edition_gets_its_audio_by_importing_again_with_the_same_labels() {
    let tmp = tempfile::tempdir().unwrap();
    let install = steam_edition_library(&tmp.path().join("library"), true);
    let state = tmp.path().join("state");
    let first = import_pc(&pc_args(&install, &state), 1).unwrap();
    assert_eq!(first.provenance.edition, "Steam Edition (appid 3837340)");
    assert!(first.files.iter().all(|f| !is_audio(&f.logical_path)));

    let workingdir = install.join("ff7/workingdir");
    add_audio(&workingdir);
    let second = import_pc(&pc_args(&install, &state), 2).unwrap();
    assert_eq!(second.set_id, first.set_id);
    let added: BTreeSet<&str> = second
        .files
        .iter()
        .filter(|f| f.entry == "added")
        .map(|f| f.logical_path.as_str())
        .collect();
    assert_eq!(added, audio_paths());
    assert!(second
        .files
        .iter()
        .filter(|f| f.entry != "added")
        .all(|f| f.entry == "unchanged"));
    assert_eq!(second.identification, first.identification);
    let m = manifest_of(&state, &second.set_id);
    assert_eq!(m.identification, Some(first.identification.clone()));
    assert_eq!(
        Library::open_existing(&state)
            .unwrap()
            .list_sets()
            .unwrap()
            .len(),
        1
    );
    let found = resolve_audio(&library_source(&state)).unwrap();
    assert!(found.music_file("bat").is_some() && found.sfx_fmt.is_some());

    // spelled as the ff7 folder or the game directory itself, it is the same set again
    let third = import_pc(&pc_args(&workingdir, &state), 3).unwrap();
    assert_eq!(third.set_id, first.set_id);
    assert!(third.files.iter().all(|f| f.entry == "unchanged"));
}

/// Adding audio to a set must not change what the manifest records about its own identification:
/// not by the audio files (an install whose only language-neutral files are audio would otherwise
/// gain the "taken as language-neutral" caveat) and not by the warnings about them.
#[test]
fn audio_never_changes_what_the_manifest_says_about_its_identification() {
    let tmp = tempfile::tempdir().unwrap();
    let install = tmp.path().join("FF7");
    // an executable and a language directory: the neutral files besides them would be audio
    touch(&install, "ff7_en.exe", b"synthetic exe");
    touch(
        &install,
        "data/lang-en/field/flevel.lgp",
        b"synthetic field archive",
    );
    let state = tmp.path().join("state");
    let first = import_pc(&pc_args(&install, &state), 1).unwrap();
    let neutral = |caveats: &[String]| caveats.iter().any(|c| c.contains("language-neutral"));
    assert!(
        !neutral(&first.identification.caveats),
        "{:?}",
        first.identification.caveats
    );
    assert!(first.warnings.is_empty(), "{:?}", first.warnings);

    // the audio arrives, with a file that is skipped (so there is an audio warning too)
    add_audio(&install);
    touch(&install, "data/Music_Ogg/odd name.ogg", b"synthetic");
    let second = import_pc(&pc_args(&install, &state), 2).unwrap();
    assert_eq!(second.set_id, first.set_id);
    assert!(
        second
            .warnings
            .iter()
            .any(|w| w.starts_with(AUDIO_WARNING_PREFIX) && w.contains("not plain")),
        "{:?}",
        second.warnings
    );
    assert_eq!(second.identification, first.identification);
    assert_eq!(
        manifest_of(&state, &second.set_id).identification,
        Some(first.identification.clone())
    );

    // a library that gets the install with its audio from the start says the very same
    let fresh = import_pc(&pc_args(&install, &tmp.path().join("fresh-state")), 3).unwrap();
    assert_eq!(fresh.set_id, first.set_id);
    assert_eq!(fresh.identification, first.identification);

    // the caveat is real for other language-neutral files, so the audio's absence from it counts
    touch(
        &install,
        "data/field/char.lgp",
        b"synthetic character archive",
    );
    let with_char = import_pc(&pc_args(&install, &tmp.path().join("char-state")), 4).unwrap();
    assert!(
        neutral(&with_char.identification.caveats),
        "{:?}",
        with_char.identification.caveats
    );
}

#[cfg(unix)]
#[test]
fn a_link_out_of_the_tree_in_an_audio_folder_costs_only_that_file() {
    use std::os::unix::fs::symlink;
    let tmp = tempfile::tempdir().unwrap();
    let install = tmp.path().join("FF7");
    let outside = tmp.path().join("elsewhere");
    game_files(&install);
    add_audio(&install);
    touch(
        &outside,
        "secret.ogg",
        b"NOT-A-REAL-SECRET: a file somewhere else on the machine",
    );
    symlink(
        outside.join("secret.ogg"),
        install.join("data/Music_Ogg/leak.ogg"),
    )
    .unwrap();
    // a link that stays inside the install is followed, and imported under the scan's name
    touch(&install, "stash/inner.ogg", b"synthetic inner track");
    symlink(
        install.join("stash/inner.ogg"),
        install.join("data/Music_Ogg/inner.ogg"),
    )
    .unwrap();

    let report = scan_pc(&install).unwrap();
    let refused: Vec<&str> = report
        .refused_links
        .iter()
        .map(|l| l.relative_path.as_str())
        .collect();
    assert_eq!(refused, ["data/Music_Ogg/leak.ogg"]);
    let state = tmp.path().join("state");
    let r = import_pc(&pc_args(&install, &state), 1).unwrap();
    let logical: BTreeSet<&str> = r.files.iter().map(|f| f.logical_path.as_str()).collect();
    assert!(!logical.contains("data/music_ogg/leak.ogg"), "{logical:?}");
    assert!(logical.contains("data/music_ogg/inner.ogg"), "{logical:?}");
    for want in audio_paths() {
        assert!(logical.contains(want), "{want}");
    }
    assert!(
        r.warnings
            .iter()
            .any(|w| w.starts_with(AUDIO_WARNING_PREFIX)
                && w.contains("symbolic link")
                && w.contains("leak.ogg")),
        "{:?}",
        r.warnings
    );
    // nothing of the outside world is in the library, and the set does not mention the link
    let secret_hash = {
        let p = tmp.path().join("secret-copy");
        fs::write(&p, fs::read(outside.join("secret.ogg")).unwrap()).unwrap();
        hash_file(&p).unwrap()
    };
    assert!(r.files.iter().all(|f| f.blake3 != secret_hash));
    assert!(!Library::open_existing(&state)
        .unwrap()
        .blob_path(&secret_hash)
        .unwrap()
        .exists());
    assert!(r
        .identification
        .caveats
        .iter()
        .all(|c| !c.contains("leak.ogg") && !c.contains("symbolic")));
    assert!(
        fs::symlink_metadata(install.join("data/Music_Ogg/leak.ogg"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

/// The import checks the real path again right before it reads a file, audio included.
#[cfg(unix)]
#[test]
fn an_audio_file_swapped_for_a_link_after_the_scan_is_refused_and_creates_nothing() {
    use super::pc::seam;
    use std::os::unix::fs::symlink;
    let tmp = tempfile::tempdir().unwrap();
    let install = tmp.path().join("FF7");
    let outside = tmp.path().join("elsewhere");
    game_files(&install);
    add_audio(&install);
    touch(&outside, "secret.dat", b"NOT-A-REAL-SECRET");
    let state = tmp.path().join("state");

    let (to, swap_in) = (
        outside.join("secret.dat"),
        install.join("data/sound/audio.dat"),
    );
    seam::AFTER_SCAN.with(|h| {
        *h.borrow_mut() = Some(Box::new(move |_dir: &Path| {
            fs::remove_file(&swap_in).unwrap();
            symlink(&to, &swap_in).unwrap();
        }));
    });
    let r = import_pc(&pc_args(&install, &state), 1);
    seam::AFTER_SCAN.with(|h| *h.borrow_mut() = None);
    let err = r.unwrap_err();
    assert!(
        matches!(&err, CommandError::SymlinkOutsideInstall { path, .. } if path == "data/sound/audio.dat"),
        "{err}"
    );
    assert!(
        !state.exists(),
        "a refusal from the install's own checks leaves no trace"
    );
}

#[test]
fn audio_alone_is_not_an_import() {
    let tmp = tempfile::tempdir().unwrap();
    let install = tmp.path().join("FF7");
    touch(&install, "ff7.exe", b"synthetic exe");
    add_audio(&install);
    // the scan sees it all and says what it would take ...
    let report = scan_pc(&install).unwrap();
    assert_eq!(report.audio.ogg_files, 2);
    assert!(!report.m1.ready);
    // ... and the import still wants the field archive
    let state = tmp.path().join("state");
    let err = import_pc(&pc_args(&install, &state), 1).unwrap_err();
    assert!(matches!(err, CommandError::NoFlevel(_)), "{err}");
    assert!(!state.exists(), "a refusal leaves no trace");
}

/// A refused link to a piece of audio is no reason for a missing field archive.
#[cfg(unix)]
#[test]
fn a_refused_audio_link_is_never_named_as_the_reason_a_flevel_is_missing() {
    use std::os::unix::fs::symlink;
    let tmp = tempfile::tempdir().unwrap();
    let outside = tmp.path().join("elsewhere");
    touch(&outside, "secret.bin", b"NOT-A-REAL-SECRET");

    // no field archive, one audio link out of the tree: the plain refusal
    let install = tmp.path().join("FF7");
    touch(&install, "ff7.exe", b"synthetic exe");
    touch(
        &install,
        "data/field/char.lgp",
        b"synthetic character archive",
    );
    touch(&install, "data/music_ogg/real.ogg", b"synthetic track");
    symlink(
        outside.join("secret.bin"),
        install.join("data/music_ogg/leak.ogg"),
    )
    .unwrap();
    let state = tmp.path().join("state");
    let err = import_pc(&pc_args(&install, &state), 1).unwrap_err();
    assert!(matches!(err, CommandError::NoFlevel(_)), "{err}");
    assert!(!state.exists());

    // the field archive is a link out of the tree too: that one is named, the audio one is not
    symlink(
        outside.join("secret.bin"),
        install.join("data/field/flevel.lgp"),
    )
    .unwrap();
    let err = import_pc(&pc_args(&install, &state), 1).unwrap_err();
    match &err {
        CommandError::NoFlevelOnlyLinks { links, .. } => {
            assert!(
                links.contains("flevel.lgp") && !links.contains("leak.ogg"),
                "{links}"
            );
        }
        other => panic!("{other}"),
    }
    assert!(!state.exists());
}

#[test]
fn names_that_are_not_plain_and_empty_files_are_left_out_of_the_set_and_the_rest_is_taken() {
    let tmp = tempfile::tempdir().unwrap();
    let install = tmp.path().join("FF7");
    game_files(&install);
    add_audio(&install);
    for odd in [
        "odd name.ogg",
        "._victory.ogg",
        "\u{e9}.ogg",
        ".hidden.ogg",
        "semi;colon.ogg",
    ] {
        touch(&install, &format!("data/Music_Ogg/{odd}"), b"synthetic");
    }
    touch(&install, "data/Music_Ogg/zero.ogg", b"");
    touch(&install, "data/Music_Ogg/readme.txt", b"not audio");
    let state = tmp.path().join("state");
    let r = import_pc(&pc_args(&install, &state), 1).unwrap();
    let logical: BTreeSet<&str> = r.files.iter().map(|f| f.logical_path.as_str()).collect();
    assert_eq!(logical.len(), GAME.len() + AUDIO.len(), "{logical:?}");
    assert!(logical.iter().all(|p| !p.contains("odd")
        && !p.contains("zero")
        && !p.contains("hidden")
        && !p.contains("readme")));
    let audio_warnings: Vec<&String> = r
        .warnings
        .iter()
        .filter(|w| w.starts_with(AUDIO_WARNING_PREFIX))
        .collect();
    assert_eq!(audio_warnings.len(), 2, "{:?}", r.warnings);
    assert!(audio_warnings[0].contains("5 file(s)") && audio_warnings[1].contains("zero.ogg"));
    assert_eq!(manifest_of(&state, &r.set_id).entries.len(), 10);
}

#[test]
fn at_most_512_audio_files_are_imported_and_the_index_files_are_among_them() {
    let tmp = tempfile::tempdir().unwrap();
    let install = tmp.path().join("FF7");
    game_files(&install);
    for i in 0..515 {
        touch(
            &install,
            &format!("data/music_ogg/t{i:03}.ogg"),
            format!("synthetic track {i}").as_bytes(),
        );
    }
    touch(&install, "data/music/music.idx", b"synthetic index");
    touch(&install, "data/sound/audio.dat", b"synthetic sound bank");
    touch(&install, "data/sound/audio.fmt", b"synthetic sound table");

    let report = scan_pc(&install).unwrap();
    assert_eq!(
        report
            .would_import
            .iter()
            .filter(|w| w.role == "audio")
            .count(),
        MAX_AUDIO_FILES
    );

    let state = tmp.path().join("state");
    let r = import_pc(&pc_args(&install, &state), 1).unwrap();
    let audio: Vec<&str> = r
        .files
        .iter()
        .map(|f| f.logical_path.as_str())
        .filter(|p| is_audio(p))
        .collect();
    assert_eq!(audio.len(), MAX_AUDIO_FILES);
    assert_eq!(r.totals.files as usize, GAME.len() + MAX_AUDIO_FILES);
    for index in [
        "data/music/music.idx",
        "data/sound/audio.dat",
        "data/sound/audio.fmt",
    ] {
        assert!(audio.contains(&index), "{index}");
    }
    // the tracks are taken in library-path order (all oggs here, so name order): 512 - 3 index
    // files = 509 of them
    assert!(
        audio.contains(&"data/music_ogg/t508.ogg") && !audio.contains(&"data/music_ogg/t509.ogg")
    );
    let cap: Vec<&String> = r
        .warnings
        .iter()
        .filter(|w| w.contains("at most 512"))
        .collect();
    assert_eq!(cap.len(), 1, "{:?}", r.warnings);
    assert!(
        cap[0].starts_with(AUDIO_WARNING_PREFIX) && cap[0].contains("6 more"),
        "{}",
        cap[0]
    );
    let m = manifest_of(&state, &r.set_id);
    assert_eq!(m.entries.len(), GAME.len() + MAX_AUDIO_FILES);
    assert!(r
        .identification
        .caveats
        .iter()
        .all(|c| !c.contains("at most 512")));
}

#[test]
fn the_state_directory_may_not_be_inside_an_audio_folder_of_the_install() {
    let tmp = tempfile::tempdir().unwrap();
    let install = tmp.path().join("FF7");
    game_files(&install);
    add_audio(&install);
    let state = install.join("data/Music_Ogg/state");
    let err = import_pc(&pc_args(&install, &state), 1).unwrap_err();
    assert!(
        matches!(err, CommandError::StateInsideSource { .. }),
        "{err}"
    );
    assert!(!state.exists(), "a refusal leaves no trace");
}
