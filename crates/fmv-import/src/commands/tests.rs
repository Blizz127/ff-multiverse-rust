//! Command-level tests on SYNTHETIC data only: fake install trees with placeholder files and
//! tiny made-up disc images. Nothing here is, or is derived from, game data.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use fmv_core::{EditionProvenance, GameId, Platform, SourceRef};
use fmv_library::{Library, LibraryError, SetHandle, SourceStamp};

use super::disc::{check_library_consistency, classify_copy, seam, Copied};
use super::pc::{canonical_lenient, recheck_sources};
use super::*;
use crate::scan::{scan_pc, LanguageError};
use crate::session::{load_session, lock_session, SessionError};
use crate::synthetic::{
    build_iso, build_iso_labeled, cue_for, lba_of, mark_form2, placeholder, steam_appmanifest,
    steam_edition_library, system_cnf, wrap_raw, write_file, RawMode, DISC_LABEL, TEST_PAYLOAD_A,
    TEST_PAYLOAD_B,
};

// ---------------------------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------------------------

fn touch(root: &Path, rel: &str, bytes: &[u8]) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, bytes).unwrap();
}

fn snapshot(root: &Path) -> BTreeMap<String, (u64, Option<SystemTime>)> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap().flatten() {
            let m = e.metadata().unwrap();
            out.insert(e.path().display().to_string(), (m.len(), m.modified().ok()));
            if m.is_dir() {
                stack.push(e.path());
            }
        }
    }
    out
}

fn pc_args(dir: &Path, state: &Path) -> ImportPcArgs {
    ImportPcArgs {
        dir: dir.to_path_buf(),
        state_dir: Some(state.to_path_buf()),
        edition: None,
        region: None,
        language: None,
        revision: None,
    }
}

/// A fake install with empty placeholder files (and one with a few synthetic bytes).
fn fake_install(root: &Path) {
    touch(root, "ff7_en.exe", b"");
    touch(root, "data/field/flevel.lgp", b"");
    touch(root, "data/field/char.lgp", b"");
    touch(root, "data/battle/battle.lgp", b"SYNTHETIC placeholder");
    touch(root, "data/battle/scene.bin", b"");
    touch(root, "data/kernel/KERNEL.BIN", b"");
    touch(root, "data/kernel/kernel2.bin", b"");
    touch(root, "data/movies/not-imported.avi", b"ignored");
}

/// A fake Steam-style install holding three languages, with different bytes per language.
fn multi_language_install(root: &Path) {
    touch(root, "ff7_en.exe", b"exe-en");
    touch(root, "ff7_fr.exe", b"exe-fr");
    touch(root, "data/field/char.lgp", b"neutral char");
    for lang in ["en", "fr", "de"] {
        touch(
            root,
            &format!("data/lang-{lang}/field/flevel.lgp"),
            format!("flevel-{lang}").as_bytes(),
        );
    }
}

fn disc_iso(exe: &str, tag: &str) -> Vec<u8> {
    let unique = format!("{tag}: synthetic data for {exe}");
    build_iso_labeled(
        DISC_LABEL,
        &[
            ("SYSTEM.CNF", &system_cnf(exe)),
            ("FIELD/MD1STIN.DAT", TEST_PAYLOAD_A),
            ("BATTLE/SCENE.BIN", TEST_PAYLOAD_B),
            ("DISC.TXT", unique.as_bytes()),
        ],
    )
}

fn write_raw(dir: &Path, name: &str, raw: &[u8]) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join(format!("{name}.bin")), raw).unwrap();
    fs::write(
        dir.join(format!("{name}.cue")),
        cue_for(&format!("{name}.bin"), "MODE2/2352"),
    )
    .unwrap();
    dir.join(format!("{name}.cue"))
}

fn write_disc(dir: &Path, name: &str, iso: &[u8]) -> PathBuf {
    write_raw(dir, name, &wrap_raw(iso, RawMode::Mode2Form1))
}

fn disc_args(image: &Path, state: &Path, set: &str) -> ImportDiscArgs {
    ImportDiscArgs {
        image: image.to_path_buf(),
        set_name: set.to_string(),
        state_dir: Some(state.to_path_buf()),
        disc: None,
        revision: None,
    }
}

fn owned(v: &[(String, Vec<u8>)]) -> Vec<(&str, &[u8])> {
    v.iter().map(|(n, d)| (n.as_str(), d.as_slice())).collect()
}

/// A disc with `n` FIELD files, the serial of `exe` and a last file (sorted) that sits on a
/// sector with a broken sync pattern when `break_last`: its import fails after the checkpoints.
/// The file CONTENT depends on `tag` only, so two images with the same tag are two rips of one
/// disc (`name` is just the image's file name).
fn big_disc(images: &Path, name: &str, tag: &str, exe: &str, n: usize, break_last: bool) -> PathBuf {
    let mut files: Vec<(String, Vec<u8>)> = (0..n)
        .map(|i| (format!("FIELD/F{i:03}.DAT"), format!("{tag} file {i}").into_bytes()))
        .collect();
    files.push(("SYSTEM.CNF".into(), system_cnf(exe)));
    files.push(("ZZ/LAST.DAT".into(), format!("last file of {tag}").into_bytes()));
    let iso = build_iso_labeled(DISC_LABEL, &owned(&files));
    let mut raw = wrap_raw(&iso, RawMode::Mode2Form1);
    if break_last {
        let lba = lba_of(&iso, "ZZ/LAST.DAT").unwrap() as usize;
        raw[lba * 2352 + 3] = 0x12;
    }
    write_raw(images, name, &raw)
}

fn manifest_of(state: &Path, set_id: &str) -> fmv_library::Manifest {
    Library::open_existing(state)
        .unwrap()
        .load_manifest(set_id)
        .unwrap()
}

#[cfg(unix)]
fn tamper(path: &Path, bytes: &[u8]) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(path, bytes).unwrap();
}

// ---------------------------------------------------------------------------------------------
// import-pc
// ---------------------------------------------------------------------------------------------

#[test]
fn import_pc_copies_needed_files_and_leaves_the_install_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let install = tmp.path().join("FF7");
    fake_install(&install);
    let state = tmp.path().join("state");
    let before = snapshot(&install);

    let r = import_pc(&pc_args(&install, &state), 1234).unwrap();
    assert_eq!(
        snapshot(&install),
        before,
        "the install tree must be byte-for-byte untouched"
    );
    assert!(r.source_files_unchanged);
    assert_eq!(r.source_evidence.files_checked, 7);
    assert!(r.source_evidence.stamps_unchanged);
    assert_eq!(r.source_evidence.content_rehash_unchanged, Some(true));
    assert_eq!(r.provenance.platform, Platform::Pc);
    assert_eq!(r.provenance.game, GameId::Ff7);
    assert_eq!(r.provenance.edition, "Steam 2013");
    assert_eq!(r.provenance.language, "en");
    assert_eq!(r.provenance.region, "unknown");
    assert_eq!(r.provenance.revision, "unknown");
    assert!(!r.provenance.modded);
    assert_eq!(r.language, "en");
    assert!(r.languages_left_out.is_empty());
    // path-free output: no install path anywhere in the printed result
    assert_eq!(r.source, SourceSummary::Install { path: None });
    let printed = serde_json::to_string(&r).unwrap();
    assert!(!printed.contains(&install.display().to_string()), "{printed}");
    // 6 data files + the executable; the .avi is not part of the import
    assert_eq!(r.totals.files, 7);
    assert_eq!(r.totals.entries_added, 7);
    // the empty placeholders (exe, flevel, char, scene, kernel, kernel2) share one blob
    assert_eq!(r.totals.blobs_created, 2, "{:?}", r.files);
    assert_eq!(r.totals.blobs_deduplicated, 5);

    // what was imported is only a guess, and it says so: in the result and in the manifest
    assert!(!r.identification.verified);
    assert_eq!(r.identification.method, "pc-file-presence");
    assert!(r.identification.caveats[0].contains("edition is a guess"));
    assert_ne!(
        r.edition_guess.confidence, "medium",
        "empty placeholder files must not read as a recognised install"
    );
    assert!(r.warnings.iter().any(|w| w.contains("empty (0-byte)")), "{:?}", r.warnings);

    let lib = Library::open_existing(&state).unwrap();
    assert_eq!(lib.list_sets().unwrap(), vec![r.set_id.clone()]);
    let m = lib.load_manifest(&r.set_id).unwrap();
    assert_eq!(m.imported_at, 1234);
    assert!(m.importer_version.starts_with("fmv-import "));
    assert_eq!(m.identification.as_ref(), Some(&r.identification));
    assert!(!m.is_verified());
    assert!(lib.get(&r.set_id, "data/field/flevel.lgp").unwrap().is_some());
    assert!(lib
        .get(&r.set_id, "data/movies/not-imported.avi")
        .unwrap()
        .is_none());
    let mut buf = Vec::new();
    lib.open_reader(&r.set_id, "DATA/BATTLE/BATTLE.LGP")
        .unwrap()
        .read_to_end(&mut buf)
        .unwrap();
    assert_eq!(buf, b"SYNTHETIC placeholder");

    // running it again changes nothing, not even one byte of the manifest
    let manifest_path = state
        .join("library")
        .join("sets")
        .join(&r.set_id)
        .join("manifest.json");
    let manifest_before = fs::read(&manifest_path).unwrap();
    let again = import_pc(&pc_args(&install, &state), 9999).unwrap();
    assert_eq!(again.totals.entries_added, 0);
    assert_eq!(again.totals.entries_unchanged, 7);
    assert_eq!(lib.load_manifest(&r.set_id).unwrap().imported_at, 1234);
    assert_eq!(fs::read(&manifest_path).unwrap(), manifest_before);
}

#[test]
fn import_pc_overrides_and_modded_installs_get_their_own_set() {
    let tmp = tempfile::tempdir().unwrap();
    let install = tmp.path().join("FF7");
    fake_install(&install);
    let state = tmp.path().join("state");
    let mut a = pc_args(&install, &state);
    a.edition = Some("PC 2012".into());
    a.region = Some("NTSC-U".into());
    a.language = Some("EN".into()); // matched case-insensitively, stored as the install spells it
    a.revision = Some("1.02".into());
    let vanilla = import_pc(&a, 1).unwrap();
    assert_eq!(
        (
            vanilla.provenance.edition.as_str(),
            vanilla.provenance.region.as_str(),
            vanilla.provenance.language.as_str(),
            vanilla.provenance.revision.as_str()
        ),
        ("PC 2012", "NTSC-U", "en", "1.02")
    );
    let caveats = vanilla.identification.caveats.join(" | ");
    for expected in ["--edition", "--region", "--language", "--revision"] {
        assert!(caveats.contains(expected), "{expected} missing from {caveats}");
    }
    touch(&install, "FFNx.dll", b"");
    let modded = import_pc(&a, 2).unwrap();
    assert!(modded.provenance.modded);
    assert_ne!(
        modded.set_id, vanilla.set_id,
        "modded installs are a distinct class"
    );
    assert_eq!(
        Library::open_existing(&state)
            .unwrap()
            .list_sets()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn an_install_without_language_markers_takes_the_label_as_given() {
    let tmp = tempfile::tempdir().unwrap();
    let install = tmp.path().join("FF7");
    touch(&install, "ff7.exe", b"exe");
    touch(&install, "data/field/flevel.lgp", b"flevel");
    let state = tmp.path().join("state");
    let plain = import_pc(&pc_args(&install, &state), 1).unwrap();
    assert_eq!(plain.provenance.language, "unknown");
    assert!(plain.identification.caveats.iter().all(|c| !c.contains("assumption")));
    let mut a = pc_args(&install, &state);
    a.language = Some("fr".into());
    let labelled = import_pc(&a, 2).unwrap();
    assert_eq!(labelled.provenance.language, "fr");
    assert_ne!(labelled.set_id, plain.set_id, "language is part of the set identity");
}

#[test]
fn a_multi_language_install_becomes_one_set_per_language_never_a_mix() {
    let tmp = tempfile::tempdir().unwrap();
    let install = tmp.path().join("steam");
    multi_language_install(&install);
    let state = tmp.path().join("state");

    // no --language: the install holds three languages and no one is implied. Refused, and
    // nothing at all was created
    let err = import_pc(&pc_args(&install, &state), 1).unwrap_err();
    assert!(
        matches!(err, CommandError::Language(LanguageError::Ambiguous { .. })),
        "{err}"
    );
    assert!(err.to_string().contains("--language") && err.to_string().contains("de, en, fr"));
    assert!(!state.exists());
    // a language the install does not hold is refused too
    let mut ja = pc_args(&install, &state);
    ja.language = Some("ja".into());
    assert!(matches!(
        import_pc(&ja, 1).unwrap_err(),
        CommandError::Language(LanguageError::NotInInstall { .. })
    ));
    assert!(!state.exists());

    let mut en = pc_args(&install, &state);
    en.language = Some("en".into());
    let en_set = import_pc(&en, 2).unwrap();
    assert_eq!(en_set.language, "en");
    assert_eq!(en_set.provenance.language, "en");
    assert_eq!(en_set.languages_left_out, ["de", "fr"]);
    assert!(en_set.warnings.iter().any(|w| w.contains("NOT imported")));
    // the classic file kept beside the language's own files is an assumption, and the manifest
    // says so (a plain classic install, with no language directories, carries no such caveat)
    assert!(
        en_set.identification.caveats.iter().any(|c| c.contains("assumption")),
        "{:?}",
        en_set.identification.caveats
    );
    let m = manifest_of(&state, &en_set.set_id);
    let names: Vec<&str> = m.entries.iter().map(|e| e.logical_path.as_str()).collect();
    assert_eq!(
        names,
        [
            "data/field/char.lgp",
            "data/lang-en/field/flevel.lgp",
            "ff7_en.exe"
        ],
        "the language-neutral file plus ONLY the English ones"
    );

    let mut fr = pc_args(&install, &state);
    fr.language = Some("fr".into());
    let fr_set = import_pc(&fr, 3).unwrap();
    assert_ne!(fr_set.set_id, en_set.set_id);
    assert_eq!(fr_set.totals.blobs_deduplicated, 1, "the neutral file is stored once");
    let lib = Library::open_existing(&state).unwrap();
    assert_eq!(lib.list_sets().unwrap().len(), 2);
    for (set, lang) in [(&en_set, "en"), (&fr_set, "fr")] {
        let mut got = String::new();
        lib.open_reader(&set.set_id, &format!("data/lang-{lang}/field/flevel.lgp"))
            .unwrap()
            .read_to_string(&mut got)
            .unwrap();
        assert_eq!(got, format!("flevel-{lang}"));
        for other in ["en", "fr", "de"].into_iter().filter(|o| *o != lang) {
            assert!(lib
                .get(&set.set_id, &format!("data/lang-{other}/field/flevel.lgp"))
                .unwrap()
                .is_none());
        }
    }
}

#[test]
fn one_language_executable_is_the_default_even_when_other_language_data_is_present() {
    let tmp = tempfile::tempdir().unwrap();
    let install = tmp.path().join("steam");
    touch(&install, "ff7_en.exe", b"exe-en");
    for lang in ["en", "fr"] {
        touch(&install, &format!("data/lang-{lang}/field/flevel.lgp"), lang.as_bytes());
    }
    let state = tmp.path().join("state");
    // what scan-pc says it would import is exactly what import-pc copies
    let report = scan_pc(&install).unwrap();
    let r = import_pc(&pc_args(&install, &state), 1).unwrap();
    let mut scanned: Vec<&str> = report
        .would_import
        .iter()
        .filter(|w| w.in_default_import)
        .map(|w| w.logical_path.as_str())
        .collect();
    let mut copied: Vec<&str> = r.files.iter().map(|f| f.logical_path.as_str()).collect();
    scanned.sort_unstable();
    copied.sort_unstable();
    assert_eq!(scanned, copied);
    assert_eq!(report.default_import_bytes, r.totals.bytes);
    assert_eq!(r.language, "en");
    assert_eq!(r.languages_left_out, ["fr"]);
    let names: Vec<String> = manifest_of(&state, &r.set_id)
        .entries
        .into_iter()
        .map(|e| e.logical_path)
        .collect();
    assert_eq!(names, ["data/lang-en/field/flevel.lgp", "ff7_en.exe"]);
}

#[test]
fn a_language_without_a_field_archive_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let install = tmp.path().join("steam");
    touch(&install, "ff7_en.exe", b"e");
    touch(&install, "ff7_fr.exe", b"f");
    touch(&install, "data/lang-fr/field/flevel.lgp", b"flevel-fr");
    let state = tmp.path().join("state");
    let mut en = pc_args(&install, &state);
    en.language = Some("en".into());
    let err = import_pc(&en, 1).unwrap_err();
    assert!(
        matches!(&err, CommandError::NoFlevelForLanguage { language, have } if language == "en" && have == "fr"),
        "{err}"
    );
    assert!(!state.exists());
}

#[test]
fn import_pc_refusals() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    // no flevel.lgp: refused, and nothing is created
    let install = tmp.path().join("FF7");
    touch(&install, "ff7.exe", b"");
    touch(&install, "data/field/char.lgp", b"");
    assert!(matches!(
        import_pc(&pc_args(&install, &state), 1),
        Err(CommandError::NoFlevel(_))
    ));
    assert!(!state.exists());
    // not a directory / missing
    assert!(import_pc(&pc_args(&tmp.path().join("missing"), &state), 1).is_err());
    // state dir inside the install (also when it does not exist yet, and via ..)
    let ok = tmp.path().join("FF7b");
    fake_install(&ok);
    for bad in [
        ok.clone(),
        ok.join("state"),
        ok.join("a/b/../c"),
        ok.join("data").join("..").join("x"),
    ] {
        let err = import_pc(&pc_args(&ok, &bad), 1).unwrap_err();
        assert!(
            matches!(err, CommandError::StateInsideSource { .. }),
            "{bad:?}: {err}"
        );
    }
    assert!(
        !ok.join("state").exists(),
        "refusal must not create the directory"
    );
    assert!(!ok.join("library").exists());
    // a sibling state dir is fine
    import_pc(&pc_args(&ok, &tmp.path().join("FF7b-state")), 1).unwrap();
}

/// `<symlink>/../state` means something else to the filesystem than to a lexical `..` fold.
#[cfg(unix)]
#[test]
fn symlinks_cannot_smuggle_the_state_directory_into_the_install() {
    let tmp = tempfile::tempdir().unwrap();
    let install = tmp.path().join("FF7");
    fake_install(&install);

    // tmp/link -> tmp/FF7/data, so tmp/link/../state is tmp/FF7/state for the OS
    std::os::unix::fs::symlink(install.join("data"), tmp.path().join("link")).unwrap();
    let sneaky = tmp.path().join("link").join("..").join("state");
    let err = import_pc(&pc_args(&install, &sneaky), 1).unwrap_err();
    assert!(matches!(err, CommandError::StateInsideSource { .. }), "{err}");
    assert!(!install.join("state").exists(), "nothing may be created in the install");

    // the state directory itself is a symlink into the install
    std::os::unix::fs::symlink(install.join("data"), tmp.path().join("linked-state")).unwrap();
    assert!(matches!(
        import_pc(&pc_args(&install, &tmp.path().join("linked-state")), 1),
        Err(CommandError::StateInsideSource { .. })
    ));

    // a good-looking state directory whose `library` is a symlink into the install
    let state = tmp.path().join("state");
    fs::create_dir_all(&state).unwrap();
    fs::create_dir_all(install.join("lib-inside")).unwrap();
    std::os::unix::fs::symlink(install.join("lib-inside"), state.join("library")).unwrap();
    assert!(matches!(
        import_pc(&pc_args(&install, &state), 1),
        Err(CommandError::StateInsideSource { .. })
    ));
    assert_eq!(fs::read_dir(install.join("lib-inside")).unwrap().count(), 0);

    // a state directory next to the install that is reached through a symlink is still fine
    std::os::unix::fs::symlink(tmp.path(), tmp.path().join("up")).unwrap();
    let fine = tmp.path().join("up").join("fine-state");
    import_pc(&pc_args(&install, &fine), 1).unwrap();
    assert!(tmp.path().join("fine-state").join("library").is_dir());
}

#[cfg(unix)]
#[test]
fn canonical_lenient_follows_symlinks_before_applying_dotdot() {
    let tmp = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(tmp.path()).unwrap();
    fs::create_dir_all(base.join("real").join("sub")).unwrap();
    std::os::unix::fs::symlink(base.join("real").join("sub"), base.join("link")).unwrap();

    // the symlink's target's parent, not the symlink's parent
    assert_eq!(canonical_lenient(&base.join("link/../x")), base.join("real/x"));
    assert_eq!(canonical_lenient(&base.join("link/..")), base.join("real"));
    // components that do not exist yet are kept, and a `..` cancels them again
    assert_eq!(canonical_lenient(&base.join("real/missing/../y")), base.join("real/y"));
    assert_eq!(canonical_lenient(&base.join("nothing/a/b/../../c")), base.join("nothing/c"));
    assert_eq!(canonical_lenient(&base.join("./real//sub")), base.join("real/sub"));
    // a symlink that only appears after a missing part is still resolved
    assert_eq!(
        canonical_lenient(&base.join("missing/../link/deeper")),
        base.join("real/sub/deeper")
    );
    assert_eq!(canonical_lenient(Path::new("/")), Path::new("/"));
}

#[test]
fn recheck_catches_a_rewrite_that_keeps_size_and_mtime() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("src.bin");
    fs::write(&f, b"original").unwrap();
    let stamp = SourceStamp::capture(&f).unwrap();
    let sources = vec![(f.clone(), stamp.clone())];
    let hashes = vec![fmv_library::hash_file(&f).unwrap()];
    recheck_sources(&sources, &hashes).unwrap();

    // rewritten in place, same size, mtime put back: the stamp alone is fooled ...
    let mtime = fs::metadata(&f).unwrap().modified().unwrap();
    let h = fs::OpenOptions::new().write(true).open(&f).unwrap();
    std::io::Write::write_all(&mut &h, b"tampered").unwrap();
    h.set_modified(mtime).unwrap();
    drop(h);
    stamp.verify(&f).unwrap();
    // ... the second read is not
    assert!(matches!(
        recheck_sources(&sources, &hashes),
        Err(LibraryError::SourceChanged(_))
    ));
    // and a moved mtime is caught by the stamp
    fs::write(&f, b"original").unwrap();
    let h = fs::OpenOptions::new().write(true).open(&f).unwrap();
    h.set_modified(mtime - Duration::from_secs(5)).unwrap();
    drop(h);
    assert!(matches!(
        recheck_sources(&sources, &hashes),
        Err(LibraryError::SourceChanged(_))
    ));
}

#[test]
fn leftovers_of_a_killed_import_are_reclaimed_and_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let install = tmp.path().join("FF7");
    fake_install(&install);
    let state = tmp.path().join("state");
    let lib = Library::open(&state).unwrap();
    let part = lib.library_dir().join("tmp").join("blob-424242-0-00000000.part");
    fs::write(&part, vec![0u8; 4096]).unwrap();
    let f = fs::OpenOptions::new().write(true).open(&part).unwrap();
    f.set_modified(SystemTime::now() - Duration::from_secs(7200)).unwrap();
    drop(f);
    drop(lib);
    let r = import_pc(&pc_args(&install, &state), 1).unwrap();
    assert!(!part.exists());
    assert!(r.warnings.iter().any(|w| w.contains("leftover temporary")), "{:?}", r.warnings);
}

#[cfg(unix)]
#[test]
fn a_damaged_blob_is_replaced_and_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let install = tmp.path().join("FF7");
    touch(&install, "ff7_en.exe", b"exe");
    touch(&install, "data/field/flevel.lgp", b"flevel bytes");
    let state = tmp.path().join("state");
    let first = import_pc(&pc_args(&install, &state), 1).unwrap();
    let lib = Library::open_existing(&state).unwrap();
    let flevel = first
        .files
        .iter()
        .find(|f| f.logical_path.ends_with("flevel.lgp"))
        .unwrap();
    let blob = lib.blob_path(&flevel.blake3).unwrap();
    tamper(&blob, b"FLEVEL BYTES"); // same size, different bytes
    let again = import_pc(&pc_args(&install, &state), 2).unwrap();
    assert_eq!(again.totals.blobs_repaired, 1);
    assert!(again.files.iter().any(|f| f.blob == "repaired"));
    assert!(again.warnings.iter().any(|w| w.contains("damaged blob")));
    assert_eq!(fs::read(&blob).unwrap(), b"flevel bytes");
    assert!(lib.verify_set(&first.set_id).unwrap().is_empty());
}

#[test]
fn scan_does_not_touch_the_state_dir() {
    // scan-pc takes no state dir at all; make sure running it creates nothing next to it
    let tmp = tempfile::tempdir().unwrap();
    let install = tmp.path().join("FF7");
    fake_install(&install);
    let state = tmp.path().join("state");
    let before = snapshot(tmp.path());
    let _ = scan_pc(&install).unwrap();
    assert_eq!(snapshot(tmp.path()), before);
    assert!(!state.exists());
    // list and status on a missing library create nothing either
    let listed = list_sets(Some(&state), false).unwrap();
    assert!(!listed.library_exists && listed.sets.is_empty());
    assert!(matches!(
        import_status("ff7", Some(&state)),
        Err(CommandError::NoSuchSession(_))
    ));
    assert!(!state.exists());
}

// ---------------------------------------------------------------------------------------------
// import-pc of the Steam Edition (appid 3837340): auto-detected labels keep the owner's set
// ---------------------------------------------------------------------------------------------

/// The id of the library set the owner's Steam Edition copy lives in. It was imported with
/// `--edition "Steam Edition (appid 3837340)" --language en --revision "build 21140793"`. A set
/// id is a directory name inside persisted libraries, and the edition/region/revision behind it
/// are bound into the area fingerprint that decides which players share an instance: neither may
/// ever drift.
const OWNERS_SET_ID: &str = "ff7-pc-steam-edition-appid-3837-unknown-build-21140793-63e94f91";

/// The six library paths of that set.
const OWNERS_SIX: [&str; 6] = [
    "data/battle/battle.lgp",
    "data/field/char.lgp",
    "data/field/flevel.lgp",
    "data/lang-en/battle/scene.bin",
    "data/lang-en/kernel/kernel.bin",
    "data/lang-en/kernel/kernel2.bin",
];

/// The arguments the owner's import used: every label spelled out.
fn owners_args(dir: &Path, state: &Path) -> ImportPcArgs {
    let mut a = pc_args(dir, state);
    a.edition = Some("Steam Edition (appid 3837340)".into());
    a.language = Some("en".into());
    a.revision = Some("build 21140793".into());
    a
}

fn paths_and_hashes(r: &ImportPcResult) -> Vec<(String, String)> {
    r.files
        .iter()
        .map(|f| (f.logical_path.clone(), f.blake3.clone()))
        .collect()
}

/// The tightest constraint of the whole feature, pinned on its own: these exact label strings
/// (and language `en`, not modded) are what the owner's set id was made from.
#[test]
fn the_owners_labels_are_pinned_to_the_owners_set_id() {
    let provenance = |edition: &str, region: &str, language: &str, revision: &str, modded: bool| {
        EditionProvenance {
            game: GameId::Ff7,
            platform: Platform::Pc,
            edition: edition.into(),
            region: region.into(),
            language: language.into(),
            revision: revision.into(),
            sources: vec![],
            modded,
        }
    };
    let owners = provenance(
        "Steam Edition (appid 3837340)",
        "unknown",
        "en",
        "build 21140793",
        false,
    );
    assert_eq!(fmv_library::set_id_for(&owners), OWNERS_SET_ID);
    // the labels the scan produces are those very strings
    assert_eq!(crate::steam::STEAM_EDITION_LABEL, owners.edition);
    assert_eq!(crate::steam::build_revision("21140793"), owners.revision);
    // every label matters: any drift is another set
    for other in [
        provenance(
            "Steam Edition (appid 3837340)",
            "unknown",
            "en",
            "build 21140793",
            true,
        ),
        provenance(
            "Steam Edition (appid 3837340)",
            "unknown",
            "fr",
            "build 21140793",
            false,
        ),
        provenance(
            "Steam Edition (appid 3837340)",
            "unknown",
            "en",
            "unknown",
            false,
        ),
        provenance(
            "Steam Edition (appid 3837340)",
            "NTSC-U",
            "en",
            "build 21140793",
            false,
        ),
        provenance("Steam Edition", "unknown", "en", "build 21140793", false),
        provenance("Steam 2013", "unknown", "en", "build 21140793", false),
    ] {
        assert_ne!(fmv_library::set_id_for(&other), OWNERS_SET_ID, "{other:?}");
    }
}

/// Fake tree + fake appmanifest in, the owner's set id and the owner's six files out, whichever
/// way the install is named and whether or not the labels are spelled out.
#[test]
fn the_steam_editions_auto_labels_reach_the_set_the_explicit_labels_made() {
    let tmp = tempfile::tempdir().unwrap();
    let install = steam_edition_library(tmp.path(), true);
    let workingdir = install.join("ff7/workingdir");
    let state = tmp.path().join("state");
    let library_before = snapshot(&tmp.path().join("steamapps"));

    // the owner's import: the labels spelled out, from the folder that holds data/
    let explicit = import_pc(&owners_args(&workingdir, &state), 1).unwrap();
    assert_eq!(explicit.set_id, OWNERS_SET_ID);
    let paths: Vec<&str> = explicit
        .files
        .iter()
        .map(|f| f.logical_path.as_str())
        .collect();
    assert_eq!(paths, OWNERS_SIX);

    // nothing spelled out, from the install root: the same set, and nothing to add to it
    let auto = import_pc(&pc_args(&install, &state), 2).unwrap();
    assert_eq!(auto.set_id, OWNERS_SET_ID);
    assert_eq!(
        auto.provenance, explicit.provenance,
        "every label is the same"
    );
    assert_eq!(auto.provenance.edition, "Steam Edition (appid 3837340)");
    assert_eq!(auto.provenance.region, "unknown");
    assert_eq!(auto.provenance.language, "en");
    assert_eq!(auto.provenance.revision, "build 21140793");
    assert!(!auto.provenance.modded);
    assert!(
        explicit.provenance.compatible_with(&auto.provenance),
        "players of the two imports share an instance"
    );
    assert_eq!(
        paths_and_hashes(&auto),
        paths_and_hashes(&explicit),
        "the same files, byte for byte (the content fingerprints)"
    );
    assert_eq!(auto.totals.entries_added, 0);
    assert_eq!(auto.totals.entries_unchanged, 6);
    assert_eq!(auto.totals.blobs_created, 0);
    assert_eq!(
        Library::open_existing(&state).unwrap().list_sets().unwrap(),
        vec![OWNERS_SET_ID.to_string()],
        "one set, not two"
    );
    assert_eq!(
        manifest_of(&state, OWNERS_SET_ID).entries.len(),
        6,
        "and no seventh entry"
    );

    // alone in a fresh library, every way of naming the install reaches that very id
    for (i, given) in [install.clone(), install.join("ff7"), workingdir.clone()]
        .into_iter()
        .enumerate()
    {
        let fresh = tmp.path().join(format!("fresh-{i}"));
        let r = import_pc(&pc_args(&given, &fresh), 3).unwrap();
        assert_eq!(r.set_id, OWNERS_SET_ID, "{given:?}");
        assert_eq!(
            paths_and_hashes(&r),
            paths_and_hashes(&explicit),
            "{given:?}"
        );
        assert!(r.source_files_unchanged);
    }
    // Steam's library, appmanifest included, was only read
    assert_eq!(snapshot(&tmp.path().join("steamapps")), library_before);
}

#[test]
fn the_steam_edition_result_says_how_it_was_labelled_and_names_no_path() {
    let tmp = tempfile::tempdir().unwrap();
    let install = steam_edition_library(tmp.path(), true);
    let state = tmp.path().join("state");
    let r = import_pc(&pc_args(&install, &state), 7).unwrap();

    // path-free output: neither the install nor the Steam library it sits in is named anywhere
    // (the state directory is: it is the user's own choice and the place the library lives)
    let printed = serde_json::to_string(&r).unwrap();
    for local in [
        install.display().to_string(),
        tmp.path().join("steamapps").display().to_string(),
    ] {
        assert!(!printed.contains(&local), "{local} in {printed}");
    }
    assert_eq!(r.source, SourceSummary::Install { path: None });

    assert_eq!(r.language, "en");
    assert_eq!(r.languages_left_out, ["de", "es", "fr", "ja"]);
    assert_eq!(r.edition_guess.confidence, "high");
    assert!(!r.edition_guess.verified && !r.identification.verified);
    assert_eq!(r.identification.method, "pc-steam-appmanifest");
    let caveats = r.identification.caveats.join(" | ");
    for expected in [
        "appmanifest_3837340.acf",
        "not compared with hashes",
        "language defaulted to en",
        "revision was read from Steam's appmanifest (build 21140793)",
        "assumption",
        "assigned to language en from file names alone",
        "[verify]",
    ] {
        assert!(
            caveats.contains(expected),
            "{expected} missing from {caveats}"
        );
    }
    assert!(!caveats.contains("given by the user"), "{caveats}");
    assert!(
        r.warnings
            .iter()
            .any(|w| w.starts_with("language defaulted to")),
        "{:?}",
        r.warnings
    );
    assert!(r
        .warnings
        .iter()
        .any(|w| w.contains("NOT imported") && w.contains("de, es, fr, ja")));

    // the manifest records exactly that, and an unchanged re-import does not rewrite it
    assert_eq!(
        manifest_of(&state, &r.set_id).identification.as_ref(),
        Some(&r.identification)
    );
    let manifest_path = state
        .join("library")
        .join("sets")
        .join(&r.set_id)
        .join("manifest.json");
    let before = fs::read(&manifest_path).unwrap();
    import_pc(&pc_args(&install, &state), 8).unwrap();
    assert_eq!(
        fs::read(&manifest_path).unwrap(),
        before,
        "same inputs, same manifest"
    );

    // labels given on the command line are recorded as the user's, and the scan's are not
    let mut given = owners_args(&install, &tmp.path().join("state2"));
    given.region = Some("NTSC-U".into());
    let g = import_pc(&given, 1).unwrap();
    let caveats = g.identification.caveats.join(" | ");
    for expected in ["--edition", "--region", "--language", "--revision"] {
        assert!(
            caveats.contains(expected),
            "{expected} missing from {caveats}"
        );
    }
    assert!(!caveats.contains("language defaulted"), "{caveats}");
    assert_eq!(
        g.identification.method, "pc-file-presence",
        "nothing was taken from Steam's record"
    );
}

/// A non-English set takes that language's own field archive. The library readers look for a
/// language's `flevel.lgp` in `data/lang-<xx>/field/`, so the variant is stored under that name.
#[test]
fn a_non_english_steam_edition_import_stores_its_field_archive_where_readers_look() {
    let tmp = tempfile::tempdir().unwrap();
    let install = steam_edition_library(tmp.path(), true);
    let state = tmp.path().join("state");
    let en = import_pc(&pc_args(&install, &state), 1).unwrap();
    let mut a = pc_args(&install, &state);
    a.language = Some("FR".into());
    let fr = import_pc(&a, 2).unwrap();

    assert_ne!(fr.set_id, en.set_id);
    assert_eq!(fr.provenance.language, "fr");
    assert_eq!(fr.provenance.edition, "Steam Edition (appid 3837340)");
    assert_eq!(fr.provenance.revision, "build 21140793");
    assert!(
        en.provenance.compatible_with(&fr.provenance),
        "same edition and build, another language"
    );
    assert_eq!(fr.languages_left_out, ["de", "en", "es", "ja"]);
    let names: Vec<String> = manifest_of(&state, &fr.set_id)
        .entries
        .into_iter()
        .map(|e| e.logical_path)
        .collect();
    assert_eq!(
        names,
        [
            "data/battle/battle.lgp",
            "data/field/char.lgp",
            "data/lang-fr/battle/scene.bin",
            "data/lang-fr/field/flevel.lgp",
            "data/lang-fr/kernel/kernel.bin",
            "data/lang-fr/kernel/kernel2.bin",
        ],
        "never the English archive"
    );
    // the French archive is the French file
    let lib = Library::open_existing(&state).unwrap();
    let mut got = Vec::new();
    lib.open_reader(&fr.set_id, "data/lang-fr/field/flevel.lgp")
        .unwrap()
        .read_to_end(&mut got)
        .unwrap();
    assert_eq!(got, placeholder("ff7/workingdir/data/field/fflevel.lgp"));
    // and the resolver the server and the client share finds each set's own archive
    for (set, file) in [(&fr, "fflevel.lgp"), (&en, "flevel.lgp")] {
        let source = fmv_library::ff7::Ff7Source::Library {
            state_dir: Some(state.clone()),
            set: Some(set.set_id.clone()),
        };
        let resolved = fmv_library::ff7::resolve_flevel(&source).unwrap();
        assert_eq!(
            fs::read(&resolved.path).unwrap(),
            placeholder(&format!("ff7/workingdir/data/field/{file}")),
            "{file}"
        );
    }
    // the manifest says the mapping is an inference from the names
    let caveats = fr.identification.caveats.join(" | ");
    for expected in [
        "fflevel.lgp",
        "language fr",
        "data/lang-fr/field/flevel.lgp",
        "[verify]",
        "chosen by the user (--language)",
    ] {
        assert!(
            caveats.contains(expected),
            "{expected} missing from {caveats}"
        );
    }
    assert!(!caveats.contains("language defaulted"), "{caveats}");

    // a language without a field archive of its own is refused, not given the English one
    fs::remove_file(install.join("ff7/workingdir/data/field/sflevel.lgp")).unwrap();
    let mut es = pc_args(&install, &tmp.path().join("state-es"));
    es.language = Some("es".into());
    assert!(matches!(
        import_pc(&es, 3).unwrap_err(),
        CommandError::NoFlevelForLanguage { .. }
    ));
    assert!(!tmp.path().join("state-es").exists());
}

#[test]
fn the_state_directory_may_not_live_in_the_steam_install_whichever_folder_was_given() {
    let tmp = tempfile::tempdir().unwrap();
    let install = steam_edition_library(tmp.path(), true);
    for (given, state) in [
        (install.clone(), install.join("state")),
        // above the folder that was given, but inside the install Steam manages
        (install.join("ff7/workingdir"), install.join("state")),
        (
            install.join("ff7/workingdir"),
            install.join("ff7/resources/state"),
        ),
        (install.join("ff7"), install.join("be_loc/state")),
    ] {
        let err = import_pc(&pc_args(&given, &state), 1).unwrap_err();
        assert!(
            matches!(err, CommandError::StateInsideSource { .. }),
            "{given:?} / {state:?}: {err}"
        );
        assert!(!state.exists(), "{state:?}");
    }
    // beside the install, in the same Steam library, is fine
    import_pc(
        &pc_args(&install.join("ff7/workingdir"), &tmp.path().join("state")),
        1,
    )
    .unwrap();
}

/// The Steam 2013 build id is evidence, not a label: 2013 sets keep the ids they always had.
#[test]
fn an_appmanifest_does_not_move_a_steam_2013_set_to_another_id() {
    let tmp = tempfile::tempdir().unwrap();
    let apps = tmp.path().join("steamapps");
    let install = apps.join("common").join("FINAL FANTASY VII");
    for f in [
        "ff7_en.exe",
        "data/lang-en/field/flevel.lgp",
        "data/lang-en/field/char.lgp",
    ] {
        touch(&install, f, f.as_bytes());
    }
    let plain = import_pc(&pc_args(&install, &tmp.path().join("s1")), 1).unwrap();
    write_file(
        &apps,
        "appmanifest_39140.acf",
        steam_appmanifest("39140", "FINAL FANTASY VII", "5012345").as_bytes(),
    );
    let with = import_pc(&pc_args(&install, &tmp.path().join("s2")), 1).unwrap();
    assert_eq!(plain.set_id, with.set_id);
    assert_eq!(with.provenance.edition, "Steam 2013");
    assert_eq!(with.provenance.revision, "unknown");
    assert_eq!(plain.edition_guess.confidence, "medium");
    assert_eq!(with.edition_guess.confidence, "high");
    assert_eq!(plain.identification.method, "pc-file-presence");
    assert_eq!(with.identification.method, "pc-steam-appmanifest");
    assert!(with.identification.caveats[0].contains("appmanifest_39140.acf"));
    // the folder name is part of the evidence: the same manifest says nothing about another folder
    let moved = apps.join("common").join("FINAL FANTASY VII copy");
    for f in ["ff7_en.exe", "data/lang-en/field/flevel.lgp"] {
        touch(&moved, f, f.as_bytes());
    }
    let r = import_pc(&pc_args(&moved, &tmp.path().join("s3")), 1).unwrap();
    assert_eq!(r.edition_guess.confidence, "medium");
    assert!(r.edition_guess.steam.is_none());
}

// ---------------------------------------------------------------------------------------------
// import-disc: sessions, identification, numbering
// ---------------------------------------------------------------------------------------------

#[test]
fn disc_session_runs_through_three_discs_and_refuses_a_foreign_one() {
    let tmp = tempfile::tempdir().unwrap();
    let images = tmp.path().join("images");
    let state = tmp.path().join("state");
    let d1 = write_disc(&images, "d1", &disc_iso("SCUS_941.63", "one"));
    let d2 = write_disc(&images, "d2", &disc_iso("SCUS_941.64", "two"));
    let d3 = write_disc(&images, "d3", &disc_iso("SCUS_941.65", "three"));
    let pal2 = write_disc(&images, "pal2", &disc_iso("SCES_108.67", "pal"));
    let img_before = snapshot(&images);

    let r1 = import_disc(&disc_args(&d1, &state, "ff7"), 100).unwrap();
    assert_eq!(r1.disc.index, 1);
    assert_eq!(r1.disc.serial, "SCUS-94163");
    assert_eq!(r1.disc.kind, DiscKind::TableMatch);
    assert!(r1.disc.serial_known && !r1.disc.serial_table_verified && !r1.disc.fixture);
    assert!(r1.disc.layout.contains("MODE2"));
    assert_eq!(r1.totals.files, 4);
    assert_eq!(r1.status.imported.len(), 1);
    assert!(r1.status.imported[0].complete);
    assert_eq!(
        r1.status.missing.iter().map(|m| m.index).collect::<Vec<_>>(),
        [2, 3]
    );
    assert_eq!(
        r1.status.missing[0].expected_serial.as_deref(),
        Some("SCUS-94164")
    );
    assert!(!r1.status.complete);
    assert_eq!(r1.status.family.region, "NTSC-U");
    assert!(r1.warnings.iter().any(|w| w.contains("UNVERIFIED")));
    // the importer's identification is recorded as unverified, in the result and the manifest
    assert!(!r1.identification.verified);
    assert_eq!(r1.identification.method, "ps1-serial-table");
    assert_eq!(r1.source_evidence.content_rehash_unchanged, None);
    assert!(r1.source_evidence.stamps_unchanged);
    assert_eq!(r1.source_evidence.files_checked, 2, "the .bin and the .cue");

    // a PAL disc 2 cannot complete an NTSC-U set
    let err = import_disc(&disc_args(&pal2, &state, "ff7"), 101).unwrap_err();
    assert!(matches!(err, CommandError::FamilyMismatch { .. }), "{err}");
    let msg = err.to_string();
    assert!(
        msg.contains("disc belongs to") && msg.contains("session is"),
        "{msg}"
    );
    assert!(
        msg.contains("PAL") && msg.contains("NTSC-U") && msg.contains("SCES-10867"),
        "{msg}"
    );
    // ... and nothing of it reached the library or the session
    let lib = Library::open_existing(&state).unwrap();
    let m = lib.load_manifest(&r1.set_id).unwrap();
    assert_eq!(m.entries.len(), 4);
    assert!(m
        .entries
        .iter()
        .all(|e| e.logical_path.starts_with("disc1/")));
    assert_eq!(
        import_status("ff7", Some(&state)).unwrap().imported.len(),
        1
    );

    let r2 = import_disc(&disc_args(&d2, &state, "ff7"), 102).unwrap();
    assert_eq!(r2.disc.index, 2);
    // identical files on disc 1 and 2 are stored once
    assert!(r2.totals.blobs_deduplicated >= 2, "{:?}", r2.totals);
    assert_eq!(r2.set_id, r1.set_id);
    let r3 = import_disc(&disc_args(&d3, &state, "ff7"), 103).unwrap();
    assert!(r3.status.complete, "{:?}", r3.status);
    assert!(r3.status.all_files_copied, "no stream file was skipped here");
    assert!(r3.status.missing.is_empty() && r3.status.incomplete.is_empty());

    let m = lib.load_manifest(&r1.set_id).unwrap();
    assert_eq!(m.entries.len(), 12);
    assert!(lib
        .get(&r1.set_id, "disc2/FIELD/MD1STIN.DAT")
        .unwrap()
        .is_some());
    assert_eq!(m.provenance.sources.len(), 3);
    for (i, serial) in [(1u8, "SCUS-94163"), (2, "SCUS-94164"), (3, "SCUS-94165")] {
        assert!(m.provenance.sources.contains(&SourceRef::Disc {
            index: i,
            serial: serial.into()
        }));
    }
    assert_eq!(m.provenance.region, "NTSC-U");
    assert_eq!(m.provenance.platform, Platform::Ps1);
    assert_eq!(m.provenance.edition, "PS1 original", "table families are shared, not scoped");
    assert_eq!(m.identification.as_ref(), Some(&r1.identification));
    assert!(!m.is_verified());
    // payload bytes match what was on the disc
    let mut buf = Vec::new();
    lib.open_reader(&r1.set_id, "disc3/BATTLE/SCENE.BIN")
        .unwrap()
        .read_to_end(&mut buf)
        .unwrap();
    assert_eq!(buf, TEST_PAYLOAD_B);

    // re-importing a disc is idempotent; the images were never modified
    let manifest_path = state
        .join("library")
        .join("sets")
        .join(&r1.set_id)
        .join("manifest.json");
    let manifest_before = fs::read(&manifest_path).unwrap();
    let again = import_disc(&disc_args(&d2, &state, "ff7"), 200).unwrap();
    assert_eq!(again.totals.entries_added, 0);
    assert_eq!(again.totals.entries_unchanged, 4);
    assert_eq!(fs::read(&manifest_path).unwrap(), manifest_before);
    assert_eq!(snapshot(&images), img_before);

    let st = import_status("ff7", Some(&state)).unwrap();
    assert!(st.complete && st.imported.len() == 3);
    assert!(!st.serial_table_verified);
    // a second session over the same family reuses the same set and the same blobs
    let other = import_disc(&disc_args(&d1, &state, "second"), 300).unwrap();
    assert_eq!(other.set_id, r1.set_id);
    assert_eq!(other.totals.entries_unchanged, 4);
}

#[test]
fn unknown_serials_are_allowed_flagged_and_private_to_their_session() {
    let tmp = tempfile::tempdir().unwrap();
    let images = tmp.path().join("images");
    let state = tmp.path().join("state");
    let a = write_disc(&images, "a", &disc_iso("SCES_999.99", "a"));
    let b = write_disc(&images, "b", &disc_iso("SCES_899.99", "b"));
    let us = write_disc(&images, "us", &disc_iso("SCUS_999.99", "us"));

    let r = import_disc(&disc_args(&a, &state, "mystery"), 1).unwrap();
    assert!(!r.disc.serial_known);
    assert_eq!(r.disc.kind, DiscKind::UnknownSerial);
    assert!(r.disc.index_assumed);
    assert_eq!(r.disc.index, 1);
    assert_eq!(r.status.family.edition, "PS1 unknown");
    // the region of an unknown serial is "unknown"; the prefix is kept as a separate, labelled guess
    assert_eq!(r.status.family.region, "unknown");
    assert_eq!(r.status.family.region_hint.as_deref(), Some("PAL"));
    assert_eq!(r.status.family.scope.as_deref(), Some("mystery"));
    assert!(r.warnings.iter().any(|w| w.contains("not in the FF7 serial table")));
    assert!(r.warnings.iter().any(|w| w.contains("ASSUMED") && w.contains("a guess")));
    assert!(r.warnings.iter().any(|w| w.contains("assumed")));
    // the manifest carries the caveats: nothing here is recorded as a fact
    let m = manifest_of(&state, &r.set_id);
    assert!(!m.is_verified());
    let ident = m.identification.as_ref().unwrap();
    assert_eq!(ident.method, "ps1-serial-unknown");
    assert!(ident.caveats.iter().any(|c| c.contains("ASSUMED")));
    assert_eq!(m.provenance.region, "unknown");
    assert_eq!(m.provenance.edition, "PS1 unknown [set mystery]");
    assert_eq!(m.provenance.game, GameId::Ff7);
    // same session, same unknown family: next free number, and the trust in --set-name is said
    let rb = import_disc(&disc_args(&b, &state, "mystery"), 2).unwrap();
    assert_eq!(rb.disc.index, 2);
    assert_eq!(rb.set_id, r.set_id);
    assert!(rb.warnings.iter().any(|w| w.contains("trusts --set-name")), "{:?}", rb.warnings);
    // the same disc again keeps its number
    let ra = import_disc(&disc_args(&a, &state, "mystery"), 3).unwrap();
    assert_eq!(ra.disc.index, 1);
    // a disc whose serial suggests another region cannot join
    assert!(matches!(
        import_disc(&disc_args(&us, &state, "mystery"), 4),
        Err(CommandError::FamilyMismatch { .. })
    ));
    // ... but it can start a session of its own, with an explicit disc number
    let mut with_disc = disc_args(&us, &state, "mystery-us");
    with_disc.disc = Some(3);
    let ru = import_disc(&with_disc, 5).unwrap();
    assert_eq!(ru.disc.index, 3);
    assert_ne!(ru.set_id, r.set_id);

    // unidentified discs never merge across sessions: the same disc in another session gets its
    // own library set
    let other = import_disc(&disc_args(&a, &state, "other-session"), 6).unwrap();
    assert_ne!(other.set_id, r.set_id);
    assert_eq!(other.totals.entries_added, 4, "not a single entry shared with the other set");
    let lib = Library::open_existing(&state).unwrap();
    assert_eq!(lib.list_sets().unwrap().len(), 3);
    assert_eq!(manifest_of(&state, &r.set_id).entries.len(), 8, "two discs of 4 files");
    assert_eq!(manifest_of(&state, &other.set_id).entries.len(), 4);

    // explicit --disc wins for unknown serials, and is checked against the table for known ones
    let known = write_disc(&images, "known", &disc_iso("SCUS_941.64", "k"));
    let mut wrong = disc_args(&known, &state, "ff7");
    wrong.disc = Some(1);
    assert!(matches!(
        import_disc(&wrong, 7),
        Err(CommandError::DiscContradiction { .. })
    ));
    wrong.disc = Some(0);
    assert!(matches!(
        import_disc(&wrong, 7),
        Err(CommandError::BadDiscNumber(0))
    ));
    wrong.disc = Some(2);
    assert_eq!(import_disc(&wrong, 7).unwrap().disc.index, 2);
    // a different disc under an already used number is refused
    let clash = write_disc(&images, "clash", &disc_iso("SCES_777.77", "c"));
    let mut c = disc_args(&clash, &state, "mystery");
    c.disc = Some(1);
    assert!(matches!(
        import_disc(&c, 8),
        Err(CommandError::DiscIndexTaken { .. })
    ));
}

/// Finding 2: with unknown serials the user types --disc for every disc, so a slip must not be
/// able to make a session look complete.
#[test]
fn the_same_unknown_disc_cannot_be_two_discs_of_a_set() {
    let tmp = tempfile::tempdir().unwrap();
    let images = tmp.path().join("images");
    let state = tmp.path().join("state");
    let a = write_disc(&images, "a", &disc_iso("SLPS_914.40", "disc one"));
    let c = write_disc(&images, "c", &disc_iso("SLPS_914.42", "disc three"));
    let mut o = disc_args(&a, &state, "uh");
    o.disc = Some(1);
    import_disc(&o, 1).unwrap();
    o.disc = Some(2); // operator slip: the SAME image again, numbered 2
    let err = import_disc(&o, 2).unwrap_err();
    assert!(
        matches!(&err, CommandError::SerialAlreadyImported { existing: 1, requested: 2, .. }),
        "{err}"
    );
    assert!(err.to_string().contains("numbered by mistake"));
    let mut o3 = disc_args(&c, &state, "uh");
    o3.disc = Some(3);
    let r = import_disc(&o3, 3).unwrap();
    assert!(!r.status.complete, "disc 2 was never imported");
    assert_eq!(r.status.missing.iter().map(|m| m.index).collect::<Vec<_>>(), [2]);
    assert_eq!(
        r.status.imported.iter().map(|d| (d.index, d.serial.as_str())).collect::<Vec<_>>(),
        [(1, "SLPS-91440"), (3, "SLPS-91442")]
    );
    // not even without --disc: the same serial keeps the number it has
    assert_eq!(import_disc(&disc_args(&a, &state, "uh"), 4).unwrap().disc.index, 1);
}

/// The library's own record backs up the session: a session that was lost (or a set shared
/// with another session) must not let two discs be merged under one number either.
#[test]
fn the_library_manifest_backs_up_the_session_checks() {
    let tmp = tempfile::tempdir().unwrap();
    let images = tmp.path().join("images");
    let state = tmp.path().join("state");
    let a = write_disc(&images, "a", &disc_iso("SLPS_914.40", "disc one"));
    let b = write_disc(&images, "b", &disc_iso("SLPS_914.41", "another disc"));
    let mut o = disc_args(&a, &state, "uh");
    o.disc = Some(1);
    let first = import_disc(&o, 1).unwrap();
    // lose the session file (the library keeps its record of "disc 1 = SLPS-91440")
    fs::remove_file(state.join("import-sessions").join("uh.json")).unwrap();

    let mut other_disc = disc_args(&b, &state, "uh");
    other_disc.disc = Some(1);
    let err = import_disc(&other_disc, 2).unwrap_err();
    assert!(
        matches!(&err, CommandError::DiscIndexTaken { session, .. } if session.contains("library set")),
        "{err}"
    );
    o.disc = Some(2); // the same disc under a second number
    let err = import_disc(&o, 3).unwrap_err();
    assert!(
        matches!(&err, CommandError::SerialAlreadyImported { session, .. } if session.contains("library set")),
        "{err}"
    );
    // neither refusal wrote a session or touched the manifest
    assert!(load_session(&state, "uh").unwrap().is_none());
    assert_eq!(manifest_of(&state, &first.set_id).entries.len(), 4);

    // the pure check, case by case
    let src = |index, serial: &str| SourceRef::Disc { index, serial: serial.into() };
    let sources = [src(1, "AAAA-11111"), src(2, "BBBB-22222"), SourceRef::Fixture];
    check_library_consistency("s", &sources, 1, "AAAA-11111").unwrap();
    check_library_consistency("s", &sources, 3, "CCCC-33333").unwrap();
    check_library_consistency("s", &[], 1, "AAAA-11111").unwrap();
    assert!(matches!(
        check_library_consistency("s", &sources, 1, "ZZZZ-99999"),
        Err(CommandError::DiscIndexTaken { index: 1, .. })
    ));
    assert!(matches!(
        check_library_consistency("s", &sources, 3, "AAAA-11111"),
        Err(CommandError::SerialAlreadyImported { existing: 1, requested: 3, .. })
    ));
}

/// Finding 1: a failed import used to leave checkpointed entries in the library and no session
/// record, so the next disc was assumed to be the same disc number and merged with the orphans.
#[test]
fn a_failed_import_leaves_a_consistent_visible_state() {
    let tmp = tempfile::tempdir().unwrap();
    let images = tmp.path().join("images");
    let state = tmp.path().join("state");
    let x = big_disc(&images, "x", "x", "SCES_999.99", 450, true);

    let err = import_disc(&disc_args(&x, &state, "mystery"), 1).unwrap_err();
    assert!(err.to_string().contains("malformed"), "{err}");
    // the library holds the 400 entries of the two checkpoints, so the session must know
    let lib = Library::open_existing(&state).unwrap();
    let set_id = lib.list_sets().unwrap().remove(0);
    let m = lib.load_manifest(&set_id).unwrap();
    assert_eq!(m.entries.len(), 400);
    assert_eq!(
        m.provenance.sources,
        [SourceRef::Disc { index: 1, serial: "SCES-99999".into() }]
    );
    let st = import_status("mystery", Some(&state)).unwrap();
    assert_eq!(st.incomplete, [1]);
    assert!(!st.complete && !st.all_files_copied);
    assert!(!st.imported[0].complete && st.imported[0].serial == "SCES-99999");
    assert!(st.notes.iter().any(|n| n.contains("did not finish")), "{:?}", st.notes);

    // a DIFFERENT disc is never merged under the number the half-done disc holds
    let y_files: Vec<(String, Vec<u8>)> = vec![
        ("SYSTEM.CNF".into(), system_cnf("SCES_888.88")),
        ("FIELD/Y000.DAT".into(), b"disc-Y file".to_vec()),
        ("FIELD/F000.DAT".into(), b"DIFFERENT content than X's F000".to_vec()),
    ];
    let y = write_disc(&images, "y", &build_iso_labeled(DISC_LABEL, &owned(&y_files)));
    let mut forced = disc_args(&y, &state, "mystery");
    forced.disc = Some(1);
    assert!(matches!(
        import_disc(&forced, 2),
        Err(CommandError::DiscIndexTaken { index: 1, .. })
    ));
    let ry = import_disc(&disc_args(&y, &state, "mystery"), 3).unwrap();
    assert_eq!(ry.disc.index, 2, "disc number 1 is reserved for the half-done disc");
    assert_eq!(ry.totals.entries_added, 3, "no entry of Y collided with X's orphans");
    let m = manifest_of(&state, &set_id);
    assert!(m.entries.iter().filter(|e| e.logical_path.starts_with("disc1/")).count() == 400);
    assert!(m.entries.iter().filter(|e| e.logical_path.starts_with("disc2/")).count() == 3);

    // retrying the same disc (a good rip this time) finishes it under the same number
    let x_good = big_disc(&images, "xgood", "x", "SCES_999.99", 450, false);
    let rx = import_disc(&disc_args(&x_good, &state, "mystery"), 4).unwrap();
    assert_eq!(rx.disc.index, 1);
    assert_eq!(rx.totals.entries_unchanged, 400, "what the failed attempt left is reused");
    assert_eq!(rx.totals.entries_added, 52);
    assert!(rx.status.incomplete.is_empty());
    assert!(rx.status.imported.iter().all(|d| d.complete));
    assert_eq!(rx.status.imported[0].files_imported, 452);
    let m = manifest_of(&state, &set_id);
    assert_eq!(m.entries.iter().filter(|e| e.logical_path.starts_with("disc1/")).count(), 452);
}

/// A disc that fails before anything reached the manifest leaves no trace at all.
#[test]
fn a_small_disc_that_fails_early_leaves_no_trace() {
    let tmp = tempfile::tempdir().unwrap();
    let images = tmp.path().join("images");
    let state = tmp.path().join("state");
    let x = big_disc(&images, "x", "x", "SCES_999.99", 3, true);
    assert!(import_disc(&disc_args(&x, &state, "mystery"), 1).is_err());
    assert!(matches!(
        import_status("mystery", Some(&state)),
        Err(CommandError::NoSuchSession(_))
    ));
    assert!(load_session(&state, "mystery").unwrap().is_none());
    assert!(
        Library::open_existing(&state).unwrap().list_sets().unwrap().is_empty(),
        "no manifest was written"
    );
    // a different disc then simply becomes disc 1
    let good = big_disc(&images, "good", "y", "SCES_888.88", 3, false);
    assert_eq!(import_disc(&disc_args(&good, &state, "mystery"), 2).unwrap().disc.index, 1);
}

/// The crash window: the manifest already holds the disc, the "complete" record is not written
/// yet. The "in progress" record must be on disk by then, or a different disc could take its number.
#[test]
fn the_intent_record_is_on_disk_before_the_final_manifest_commit() {
    let tmp = tempfile::tempdir().unwrap();
    let images = tmp.path().join("images");
    let state = tmp.path().join("state");
    let a = write_disc(&images, "a", &disc_iso("SCES_999.99", "a"));
    let seen = std::rc::Rc::new(std::cell::RefCell::new(None));
    let probe = seen.clone();
    seam::AFTER_COMMIT.with(|h| {
        *h.borrow_mut() = Some(Box::new(move |state_dir: &Path| {
            let session = load_session(state_dir, "mystery").unwrap();
            let lib = Library::open_existing(state_dir).unwrap();
            let set_id = lib.list_sets().unwrap().remove(0);
            let manifest = lib.load_manifest(&set_id).unwrap();
            *probe.borrow_mut() = Some((session, manifest.entries.len(), manifest.provenance.sources));
        }));
    });
    let r = import_disc(&disc_args(&a, &state, "mystery"), 1);
    seam::AFTER_COMMIT.with(|h| *h.borrow_mut() = None);
    r.unwrap();
    let (session, entries, sources) = seen.borrow_mut().take().expect("the hook did not run");
    assert_eq!(entries, 4, "the manifest already holds the disc ...");
    assert_eq!(sources.len(), 1);
    let session = session.expect("... so the session must already know about it");
    assert_eq!(session.discs.len(), 1);
    assert!(!session.discs[0].complete, "as an unfinished disc");
    assert_eq!(session.incomplete_discs(), [1]);
    // and the import then completes the record
    let done = import_status("mystery", Some(&state)).unwrap();
    assert!(done.incomplete.is_empty() && done.imported[0].complete);
}

#[test]
fn refusals_that_need_no_lock_leave_no_trace() {
    let tmp = tempfile::tempdir().unwrap();
    let images = tmp.path().join("images");
    let state = tmp.path().join("state");
    let d1 = write_disc(&images, "d1", &disc_iso("SCUS_941.63", "one"));
    let d2 = write_disc(&images, "d2", &disc_iso("SCUS_941.64", "two"));
    let pal = write_disc(&images, "pal", &disc_iso("SCES_108.67", "pal"));
    let mut u = disc_args(&write_disc(&images, "u", &disc_iso("SLPS_914.40", "u")), &state, "uh");
    u.disc = Some(1);
    import_disc(&u, 1).unwrap();
    import_disc(&disc_args(&d1, &state, "ff7"), 2).unwrap();
    let before = snapshot(&state);

    let mut wrong_number = disc_args(&d2, &state, "ff7");
    wrong_number.disc = Some(1);
    let mut bad_revision = disc_args(&d2, &state, "ff7");
    bad_revision.revision = Some("9.9".into());
    let mut twice = u.clone();
    twice.disc = Some(2);
    let junk = tmp.path().join("junk.iso");
    fs::write(&junk, vec![7u8; 100_000]).unwrap();
    for (what, args) in [
        ("foreign region", disc_args(&pal, &state, "ff7")),
        ("table contradiction", wrong_number),
        ("revision", bad_revision),
        ("same serial, second number", twice),
        ("not an image", disc_args(&junk, &state, "ff7")),
    ] {
        assert!(import_disc(&args, 9).is_err(), "{what}");
        assert_eq!(snapshot(&state), before, "{what}: a refusal must leave no trace");
    }
}

#[test]
fn a_session_that_another_import_holds_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let images = tmp.path().join("images");
    let state = tmp.path().join("state");
    let d1 = write_disc(&images, "d1", &disc_iso("SCUS_941.63", "one"));
    let hold = lock_session(&state, "ff7").unwrap();
    let err = import_disc(&disc_args(&d1, &state, "ff7"), 1).unwrap_err();
    assert!(matches!(err, CommandError::Session(SessionError::Busy(_))), "{err}");
    assert!(!state.join("library").exists(), "nothing was imported");
    drop(hold);
    import_disc(&disc_args(&d1, &state, "ff7"), 2).unwrap();
}

// ---------------------------------------------------------------------------------------------
// import-disc: what is skipped, what is fixture, what is fatal
// ---------------------------------------------------------------------------------------------

#[test]
fn xa_stream_files_are_skipped_reported_and_not_counted_as_copied() {
    let tmp = tempfile::tempdir().unwrap();
    let images = tmp.path().join("images");
    let state = tmp.path().join("state");
    let iso = disc_iso("SCUS_941.63", "xa");
    let mut raw = wrap_raw(&iso, RawMode::Mode2Form1);
    mark_form2(&mut raw, lba_of(&iso, "BATTLE/SCENE.BIN").unwrap() as usize);
    fs::create_dir_all(&images).unwrap();
    fs::write(images.join("xa.bin"), raw).unwrap();
    let r = import_disc(&disc_args(&images.join("xa.bin"), &state, "xa"), 1).unwrap();
    assert_eq!(r.skipped_xa_count, 1);
    assert_eq!(r.skipped_xa, ["BATTLE/SCENE.BIN"]);
    assert_eq!(r.totals.files, 3);
    assert_eq!(r.status.imported[0].skipped_xa, 1);
    assert_eq!(r.skipped_other_count, 0);
    assert!(
        r.status.needs_attention.is_empty(),
        "XA skips are expected, not an anomaly"
    );
    assert!(r.warnings.iter().any(|w| w.contains("known deviation")), "{:?}", r.warnings);
    let lib = Library::open_existing(&state).unwrap();
    assert!(lib
        .get(&r.set_id, "disc1/BATTLE/SCENE.BIN")
        .unwrap()
        .is_none());
    assert!(lib
        .get(&r.set_id, "disc1/FIELD/MD1STIN.DAT")
        .unwrap()
        .is_some());
    // no half-written temp blobs
    assert_eq!(
        fs::read_dir(lib.library_dir().join("tmp")).unwrap().count(),
        0
    );

    // all three discs with a skipped stream: complete (the data the milestones need is there),
    // but NOT byte-complete, and the status says so instead of staying silent
    let mut last = None;
    for (n, exe) in ["SCUS_941.63", "SCUS_941.64", "SCUS_941.65"].into_iter().enumerate() {
        let iso = disc_iso(exe, "xa");
        let mut raw = wrap_raw(&iso, RawMode::Mode2Form1);
        mark_form2(&mut raw, lba_of(&iso, "BATTLE/SCENE.BIN").unwrap() as usize);
        let img = write_raw(&images, &format!("full{n}"), &raw);
        last = Some(import_disc(&disc_args(&img, &state, "full"), 10 + n as u64).unwrap());
    }
    let st = last.unwrap().status;
    assert!(st.complete);
    assert!(!st.all_files_copied);
    assert_eq!(st.skipped_xa_total, 3);
    assert!(st.notes.iter().any(|n| n.contains("deviation")), "{:?}", st.notes);
    assert!(import_status("full", Some(&state)).unwrap().skipped_xa_total == 3);
}

#[test]
fn images_labelled_synthetic_are_fixture_content_never_ff7() {
    let tmp = tempfile::tempdir().unwrap();
    let images = tmp.path().join("images");
    let state = tmp.path().join("state");
    // `build_iso` labels its images SYNTHETIC_TEST; this one even claims the FF7 serial
    let iso = build_iso(&[
        ("SYSTEM.CNF", &system_cnf("SCUS_941.63")),
        ("FIELD/MD1STIN.DAT", TEST_PAYLOAD_A),
    ]);
    let img = write_disc(&images, "fake", &iso);
    let r = import_disc(&disc_args(&img, &state, "fixture-run"), 1).unwrap();
    assert_eq!(r.disc.kind, DiscKind::Fixture);
    assert!(r.disc.fixture);
    assert!(!r.disc.serial_known, "SCUS-94163 is in the table, but a fixture is not a table match");
    assert!(!r.status.family.serial_known);
    assert!(r.warnings.iter().any(|w| w.contains("FIXTURE") && w.contains("NOT as FINAL FANTASY VII")));
    let m = manifest_of(&state, &r.set_id);
    assert_eq!(m.provenance.game, GameId::Fixture);
    assert_eq!(m.provenance.platform, Platform::Synthetic);
    assert_eq!(m.provenance.edition, "fixture [set fixture-run]");
    assert_eq!(m.identification.as_ref().unwrap().method, "synthetic-volume-label");
    assert!(!m.is_verified());
    assert_eq!(r.status.family.game, GameId::Fixture);
    // the serial table does not speak for fixtures: numbering follows the unknown-serial rules
    assert_eq!(r.disc.index, 1);
    assert!(r.disc.index_assumed);
    // `list` labels it FIXTURE
    let listed = list_sets(Some(&state), false).unwrap();
    assert!(listed.sets[0].fixture);
    assert!(listed.sets[0].label.as_deref().unwrap().contains("FIXTURE"));
    // a real-looking disc cannot join a fixture session
    let real = write_disc(&images, "real", &disc_iso("SCUS_941.64", "real"));
    assert!(matches!(
        import_disc(&disc_args(&real, &state, "fixture-run"), 2),
        Err(CommandError::FamilyMismatch { .. })
    ));
}

/// Finding 13b: the walker lists what it can; one unusable name must not abort the whole disc,
/// but a conflict with content already in the set stays fatal.
#[test]
fn unusable_names_are_skipped_and_flag_the_disc_but_conflicts_stay_fatal() {
    let tmp = tempfile::tempdir().unwrap();
    let images = tmp.path().join("images");
    let state = tmp.path().join("state");
    let long = "D".repeat(200);
    let deep = format!("{long}/{long}/{long}/LEAF.DAT");
    let files: Vec<(String, Vec<u8>)> = vec![
        ("SYSTEM.CNF".into(), system_cnf("SCUS_941.63")),
        ("FIELD/OK.DAT".into(), b"fine".to_vec()),
        (deep, b"too deep to store".to_vec()),
        ("DUP.TXT".into(), b"first version".to_vec()),
        ("DUP.TXT.".into(), b"another file with the same cleaned name".to_vec()),
    ];
    let img = write_disc(&images, "a", &build_iso_labeled(DISC_LABEL, &owned(&files)));
    let r = import_disc(&disc_args(&img, &state, "ff7"), 1).unwrap();
    assert!(r.skipped_other_count >= 2, "{:?}", r.skipped_other);
    assert!(r.skipped_other.iter().any(|s| s.reason.contains("longer than")));
    assert!(r.skipped_other.iter().any(|s| s.reason.contains("same name")));
    assert_eq!(r.status.needs_attention, [1], "the disc is flagged");
    assert!(!r.status.complete);
    let lib = Library::open_existing(&state).unwrap();
    assert!(lib.get(&r.set_id, "disc1/FIELD/OK.DAT").unwrap().is_some());
    assert!(lib.get(&r.set_id, "disc1/SYSTEM.CNF").unwrap().is_some());
    assert!(lib.get(&r.set_id, "disc1/DUP.TXT").unwrap().is_some());

    // a different rip of the SAME disc whose file differs: a conflict with what the set holds
    let d1 = write_disc(&images, "d1", &disc_iso("SCUS_941.63", "one"));
    import_disc(&disc_args(&d1, &state, "ok"), 2).unwrap();
    let changed = build_iso_labeled(
        DISC_LABEL,
        &[
            ("SYSTEM.CNF", &system_cnf("SCUS_941.63")),
            ("FIELD/MD1STIN.DAT", b"a different version of this file"),
            ("BATTLE/SCENE.BIN", TEST_PAYLOAD_B),
            ("DISC.TXT", b"one: synthetic data for SCUS_941.63"),
        ],
    );
    let d1b = write_disc(&images, "d1b", &changed);
    let manifest_before = manifest_of(&state, &r.set_id).entries.len();
    let err = import_disc(&disc_args(&d1b, &state, "ok"), 3).unwrap_err();
    assert!(
        matches!(err, CommandError::Library(LibraryError::EntryConflict { .. })),
        "{err}"
    );
    let st = import_status("ok", Some(&state)).unwrap();
    assert!(st.imported[0].complete, "the earlier, good import is untouched");
    assert_eq!(manifest_of(&state, &r.set_id).entries.len(), manifest_before);
}

/// Only trouble about one file's name or kind is carried on from; everything else stops the import.
#[test]
fn only_unstorable_names_and_streams_are_skipped_everything_else_is_fatal() {
    use crate::iso9660::Form2Sector;
    let outcome = || fmv_library::ImportOutcome {
        logical_path: "disc1/A".into(),
        blake3: "0".repeat(64),
        size: 1,
        blob_created: true,
        blob_repaired: false,
        entry: fmv_library::EntryStatus::Added,
    };
    let io_other = || std::io::Error::other("disk on fire");
    assert!(matches!(classify_copy(Ok(outcome()), "A").unwrap(), Copied::Imported(_)));
    let form2 = std::io::Error::new(std::io::ErrorKind::InvalidData, Form2Sector { lba: 5 });
    assert!(matches!(
        classify_copy(Err(LibraryError::SourceRead(form2)), "STR/X.STR").unwrap(),
        Copied::Stream
    ));
    let bad_name = LibraryError::BadLogicalPath { path: "disc1/x".into(), reason: "path too long" };
    match classify_copy(Err(bad_name), "DEEP/X.DAT").unwrap() {
        Copied::Unstorable(e) => {
            assert_eq!(e.path, "DEEP/X.DAT");
            assert!(e.reason.contains("path too long"));
        }
        other => panic!("{other:?}"),
    }
    for fatal in [
        LibraryError::EntryConflict {
            set_id: "s".into(),
            logical_path: "disc1/A".into(),
            existing: "a".into(),
            incoming: "b".into(),
        },
        LibraryError::SourceRead(io_other()),
        LibraryError::TooLarge { what: "x".into(), size: 2, limit: 1 },
        LibraryError::SizeMismatch { logical_path: "x".into(), expected: 1, actual: 2 },
        LibraryError::TooManyEntries { set_id: "s".into(), limit: 1 },
        LibraryError::SetBusy("s".into()),
    ] {
        let text = fatal.to_string();
        assert!(classify_copy(Err(fatal), "A").is_err(), "{text} must stop the import");
    }
}

#[test]
fn plain_iso_images_and_bad_images() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    fs::write(tmp.path().join("d1.iso"), disc_iso("SCUS_941.63", "iso")).unwrap();
    let r = import_disc(&disc_args(&tmp.path().join("d1.iso"), &state, "ff7"), 1).unwrap();
    assert_eq!(r.disc.layout, "plain 2048-byte sectors");
    assert!(r.warnings.iter().any(|w| w.contains("plain 2048-byte")));

    // an ISO without SYSTEM.CNF is not a PlayStation disc
    fs::write(
        tmp.path().join("data.iso"),
        build_iso(&[("README.TXT", b"hi")]),
    )
    .unwrap();
    let err =
        import_disc(&disc_args(&tmp.path().join("data.iso"), &state, "other"), 1).unwrap_err();
    assert!(matches!(err, CommandError::NotPlayStation(_)), "{err}");
    // SYSTEM.CNF without a usable BOOT line
    fs::write(
        tmp.path().join("badcnf.iso"),
        build_iso(&[("SYSTEM.CNF", b"TCB = 4\r\n")]),
    )
    .unwrap();
    assert!(matches!(
        import_disc(
            &disc_args(&tmp.path().join("badcnf.iso"), &state, "other"),
            1
        ),
        Err(CommandError::NotPlayStation(_))
    ));
    // garbage, missing, bad names
    fs::write(tmp.path().join("junk.iso"), vec![7u8; 100_000]).unwrap();
    assert!(matches!(
        import_disc(&disc_args(&tmp.path().join("junk.iso"), &state, "other"), 1),
        Err(CommandError::Image(_))
    ));
    assert!(import_disc(&disc_args(&tmp.path().join("nope.cue"), &state, "other"), 1).is_err());
    assert!(matches!(
        import_disc(&disc_args(&tmp.path().join("d1.iso"), &state, "../evil"), 1),
        Err(CommandError::Session(SessionError::BadName(_)))
    ));
    // none of the failures created a session
    assert!(load_session(&state, "other").unwrap().is_none());
    // an image inside the library is refused
    let lib = Library::open(&state).unwrap();
    let inside = lib.library_dir().join("sets").join("copy.iso");
    fs::copy(tmp.path().join("d1.iso"), &inside).unwrap();
    let err = import_disc(&disc_args(&inside, &state, "inside"), 1).unwrap_err();
    assert!(
        matches!(
            err,
            CommandError::Library(LibraryError::SourceInsideLibrary(_))
        ),
        "{err}"
    );
}

#[test]
fn revision_is_fixed_by_the_first_disc() {
    let tmp = tempfile::tempdir().unwrap();
    let images = tmp.path().join("images");
    let state = tmp.path().join("state");
    let d1 = write_disc(&images, "d1", &disc_iso("SCUS_941.63", "one"));
    let d2 = write_disc(&images, "d2", &disc_iso("SCUS_941.64", "two"));
    let mut a = disc_args(&d1, &state, "ff7");
    a.revision = Some("1.1".into());
    let r1 = import_disc(&a, 1).unwrap();
    assert_eq!(r1.status.family.revision, "1.1");
    // omitting --revision later inherits the session's revision
    let r2 = import_disc(&disc_args(&d2, &state, "ff7"), 2).unwrap();
    assert_eq!(r2.set_id, r1.set_id);
    // contradicting it is refused
    let mut c = disc_args(&d2, &state, "ff7");
    c.revision = Some("2.0".into());
    assert!(matches!(
        import_disc(&c, 3),
        Err(CommandError::RevisionMismatch { .. })
    ));
}

// ---------------------------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------------------------

#[test]
fn list_is_path_free_by_default_labels_fixtures_and_survives_damaged_manifests() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let install = tmp.path().join("secret-user-name-FF7");
    fake_install(&install);
    let pc = import_pc(&pc_args(&install, &state), 5).unwrap();
    // a synthetic fixture set put there by someone else
    let lib = Library::open(&state).unwrap();
    let fixture = EditionProvenance {
        game: GameId::Fixture,
        platform: Platform::Synthetic,
        edition: "fixture".into(),
        region: "none".into(),
        language: "none".into(),
        revision: "v1".into(),
        sources: vec![],
        modded: false,
    };
    let src = tmp.path().join("fixture.bin");
    fs::write(&src, b"synthetic").unwrap();
    lib.import_file(
        &SetHandle::new(fixture),
        "plaza.bin",
        &src,
        SourceRef::Fixture,
        6,
    )
    .unwrap();
    // a PC set that only the platform marks as synthetic: the label must still say FIXTURE
    let marked = EditionProvenance {
        game: GameId::Ff7,
        platform: Platform::Synthetic,
        edition: "pretend".into(),
        region: "none".into(),
        language: "none".into(),
        revision: "v1".into(),
        sources: vec![],
        modded: false,
    };
    lib.import_file(&SetHandle::new(marked), "x.bin", &src, SourceRef::Fixture, 6)
        .unwrap();
    // and a set whose manifest is damaged
    let broken = lib.set_dir("ff7-pc-broken-00000000").unwrap();
    fs::create_dir_all(&broken).unwrap();
    fs::write(broken.join("manifest.json"), b"{ not json").unwrap();

    let listed = list_sets(Some(&state), false).unwrap();
    assert!(listed.library_exists);
    assert_eq!(listed.sets.len(), 4);
    let fixtures: Vec<_> = listed.sets.iter().filter(|s| s.fixture).collect();
    assert_eq!(fixtures.len(), 2);
    assert!(fixtures.iter().all(|s| s.label.as_deref().unwrap().contains("FIXTURE")));
    let pc_set = listed.sets.iter().find(|s| s.set_id == pc.set_id).unwrap();
    assert!(!pc_set.fixture);
    assert_eq!(pc_set.label.as_deref(), Some("FINAL FANTASY VII"));
    assert_eq!(pc_set.files, Some(7));
    assert_eq!(pc_set.summary.as_ref().unwrap().edition, "Steam 2013");
    assert_eq!(pc_set.identification_verified, Some(false));
    assert!(pc_set.identification.as_ref().unwrap().caveats.len() >= 4);
    let broken_set = listed.sets.iter().find(|s| s.error.is_some()).unwrap();
    assert!(broken_set.files.is_none() && broken_set.summary.is_none());

    // path-free by default: the install path (with the user's name in it) is not printed ...
    assert_eq!(pc_set.sources, [SourceSummary::Install { path: None }]);
    let json = serde_json::to_string(&listed).unwrap();
    assert!(!json.contains("secret-user-name-FF7"), "{json}");
    // ... and the flat summary fields are still there for scripts
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    let first_pc = v["sets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["set_id"] == pc.set_id.as_str())
        .unwrap();
    assert_eq!(first_pc["edition"], "Steam 2013");
    assert_eq!(first_pc["modded"], false);
    assert_eq!(first_pc["identification_verified"], false);
    // --show-paths opts in
    let with_paths = list_sets(Some(&state), true).unwrap();
    let json = serde_json::to_string(&with_paths).unwrap();
    assert!(json.contains("secret-user-name-FF7"), "{json}");
}
