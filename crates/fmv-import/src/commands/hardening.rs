//! Tests for the hardening of findings 6-10 of the security audit, on SYNTHETIC data only:
//! symbolic links in PC installs and disc images, crafted discs that claim a genuine serial,
//! `--replace-disc`, `remove-set`, private state, locks and leftovers of killed writers.
//! Nothing here is, or is derived from, game data; every path is a temp dir.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use fmv_core::SourceRef;
use fmv_library::{pretend_locks_are_unsupported, Library};

use super::tests::{
    big_disc, disc_args, disc_iso, fake_install, manifest_of, owned, pc_args, snapshot, touch,
    write_disc, write_raw,
};
use super::*;
use crate::scan::scan_pc;
use crate::session::{load_session, lock_session, SessionError};
use crate::synthetic::{
    build_iso_labeled, mark_form2, system_cnf, wrap_raw, RawMode, DISC_LABEL, TEST_PAYLOAD_A,
};

fn count_files(dir: &Path) -> usize {
    let mut n = 0;
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.path().is_dir() {
                n += count_files(&e.path());
            } else {
                n += 1;
            }
        }
    }
    n
}

fn manifest_file(state: &Path, set_id: &str) -> PathBuf {
    state
        .join("library/sets")
        .join(set_id)
        .join("manifest.json")
}

fn set_ids(state: &Path) -> Vec<String> {
    Library::open_existing(state).unwrap().list_sets().unwrap()
}

/// Resets this thread's "the filesystem has no advisory locks" switch even when a test fails.
struct NoLocks;
impl NoLocks {
    fn on() -> NoLocks {
        pretend_locks_are_unsupported(true);
        NoLocks
    }
}
impl Drop for NoLocks {
    fn drop(&mut self) {
        pretend_locks_are_unsupported(false);
    }
}

fn backdate(path: &Path, secs: u64) {
    let f = fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_modified(SystemTime::now() - Duration::from_secs(secs))
        .unwrap();
}

/// Rewrite a manifest without its `discs` records, as a library written before they existed.
fn strip_disc_records(state: &Path, set_id: &str) {
    let path = manifest_file(state, set_id);
    let mut v: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert!(
        v.as_object_mut().unwrap().remove("discs").is_some(),
        "the manifest had no disc records"
    );
    fs::write(&path, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
}

// ---------------------------------------------------------------------------------------------
// finding 8: symbolic links
// ---------------------------------------------------------------------------------------------

const SECRET: &[u8] = b"NOT-A-REAL-SECRET: a file somewhere else on the machine";

/// A small install with real files; `flevel.lgp` is left to the caller.
fn small_install(root: &Path) {
    touch(root, "ff7.exe", b"exe bytes");
    touch(root, "data/field/char.lgp", b"char bytes");
    touch(root, "data/battle/scene.bin", b"scene bytes");
}

#[cfg(unix)]
#[test]
fn a_link_out_of_the_install_is_left_out_by_the_scan_and_refused_by_the_import() {
    use std::os::unix::fs::symlink;
    let tmp = tempfile::tempdir().unwrap();
    let install = tmp.path().join("FF7");
    let outside = tmp.path().join("elsewhere");
    small_install(&install);
    touch(&outside, "secret.txt", SECRET);
    symlink(
        outside.join("secret.txt"),
        install.join("data/field/flevel.lgp"),
    )
    .unwrap();

    // the scan: the link is not a candidate, it is listed, and the report says so
    let report = scan_pc(&install).unwrap();
    assert!(
        !report
            .would_import
            .iter()
            .any(|w| w.logical_path.ends_with("flevel.lgp")),
        "{:?}",
        report.would_import
    );
    assert_eq!(report.refused_links.len(), 1, "{:?}", report.refused_links);
    assert_eq!(
        report.refused_links[0].relative_path,
        "data/field/flevel.lgp"
    );
    assert!(report.refused_links[0].reason.contains("outside"));
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("symbolic link") && w.contains("flevel.lgp")),
        "{:?}",
        report.warnings
    );
    assert!(
        !report.m1.ready,
        "a link that is not followed is not a flevel.lgp"
    );
    let printed = serde_json::to_string(&report).unwrap();
    assert!(printed.contains("refused_links"), "{printed}");
    assert!(
        !printed.contains(&outside.display().to_string()),
        "no local path of the target is reported"
    );

    // the import: refused up front, with the reason, and nothing is created anywhere
    let state = tmp.path().join("state");
    let err = import_pc(&pc_args(&install, &state), 1).unwrap_err();
    assert!(
        matches!(&err, CommandError::NoFlevelOnlyLinks { links, .. } if links.contains("flevel.lgp")),
        "{err}"
    );
    assert!(err.to_string().contains("symbolic link"), "{err}");
    assert!(!state.exists(), "a refusal leaves no trace");
    assert_eq!(fs::read(outside.join("secret.txt")).unwrap(), SECRET);
}

#[cfg(unix)]
#[test]
fn a_refused_link_next_to_a_real_flevel_costs_only_that_file() {
    use std::os::unix::fs::symlink;
    let tmp = tempfile::tempdir().unwrap();
    let install = tmp.path().join("FF7");
    let outside = tmp.path().join("elsewhere");
    small_install(&install);
    touch(&install, "data/field/flevel.lgp", b"the real field archive");
    touch(&outside, "secret.txt", SECRET);
    fs::remove_file(install.join("data/battle/scene.bin")).unwrap();
    symlink(
        outside.join("secret.txt"),
        install.join("data/battle/scene.bin"),
    )
    .unwrap();
    // a link to a DIRECTORY out of the tree takes every file behind it with it
    fs::create_dir_all(outside.join("kernel")).unwrap();
    touch(&outside, "kernel/kernel.bin", b"also outside");
    symlink(outside.join("kernel"), install.join("data/kernel")).unwrap();

    let state = tmp.path().join("state");
    let r = import_pc(&pc_args(&install, &state), 1).unwrap();
    let paths: BTreeSet<&str> = r.files.iter().map(|f| f.logical_path.as_str()).collect();
    assert!(paths.contains("data/field/flevel.lgp") && paths.contains("data/field/char.lgp"));
    assert!(
        !paths.contains("data/battle/scene.bin") && !paths.contains("data/kernel/kernel.bin"),
        "{paths:?}"
    );
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("symbolic link") && w.contains("scene.bin")),
        "{:?}",
        r.warnings
    );
    // nothing of the outside world is in the library: not as a blob, not as an entry
    let secret_hash = {
        let p = tmp.path().join("secret-copy");
        fs::write(&p, SECRET).unwrap();
        fmv_library::hash_file(&p).unwrap()
    };
    assert!(r.files.iter().all(|f| f.blake3 != secret_hash));
    let lib = Library::open_existing(&state).unwrap();
    assert!(!lib.blob_path(&secret_hash).unwrap().exists());
    assert_eq!(fs::read(outside.join("secret.txt")).unwrap(), SECRET);
}

/// A link that stays inside the install is followed, and the set is exactly what the same files
/// without the link give: logical paths come from the scan's names, not from where links lead.
#[cfg(unix)]
#[test]
fn links_inside_the_install_are_followed_and_change_nothing_about_the_set() {
    use std::os::unix::fs::symlink;
    let tmp = tempfile::tempdir().unwrap();
    let plain = tmp.path().join("plain");
    let linked = tmp.path().join("linked");
    for root in [&plain, &linked] {
        small_install(root);
    }
    touch(&plain, "data/field/flevel.lgp", b"the real field archive");
    // the same bytes, but reached through a link (and a linked directory) inside the install
    touch(&linked, "stash/real-flevel.bin", b"the real field archive");
    symlink(
        linked.join("stash/real-flevel.bin"),
        linked.join("data/field/flevel.lgp"),
    )
    .unwrap();
    symlink("../../stash", linked.join("data/lang-alias")).unwrap(); // not a language directory

    let a = import_pc(&pc_args(&plain, &tmp.path().join("state-a")), 1).unwrap();
    let b = import_pc(&pc_args(&linked, &tmp.path().join("state-b")), 1).unwrap();
    let shape = |r: &ImportPcResult| {
        r.files
            .iter()
            .map(|f| (f.logical_path.clone(), f.blake3.clone(), f.size))
            .collect::<Vec<_>>()
    };
    assert_eq!(shape(&a), shape(&b));
    assert_eq!(a.set_id, b.set_id);
    assert!(
        b.warnings.iter().all(|w| !w.contains("symbolic link")),
        "{:?}",
        b.warnings
    );
    let ma = manifest_of(&tmp.path().join("state-a"), &a.set_id);
    let mb = manifest_of(&tmp.path().join("state-b"), &b.set_id);
    let entries = |m: &fmv_library::Manifest| {
        m.entries
            .iter()
            .map(|e| (e.logical_path.clone(), e.blake3.clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(entries(&ma), entries(&mb));
    // the recorded source is the install root, canonical, whatever the links inside it do
    let root = fs::canonicalize(&linked).unwrap().display().to_string();
    assert_eq!(
        mb.provenance.sources,
        vec![SourceRef::Install { path: root }]
    );
    // the link itself and the file it leads to are untouched
    assert!(fs::symlink_metadata(linked.join("data/field/flevel.lgp"))
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(
        fs::read(linked.join("stash/real-flevel.bin")).unwrap(),
        b"the real field archive"
    );
}

#[cfg(unix)]
#[test]
fn an_install_reached_through_a_link_is_fine_and_a_link_swapped_in_after_the_scan_is_caught() {
    use std::os::unix::fs::symlink;
    let tmp = tempfile::tempdir().unwrap();
    let real = tmp.path().join("real-install");
    small_install(&real);
    touch(&real, "data/field/flevel.lgp", b"the real field archive");
    let via = tmp.path().join("via-link");
    symlink(&real, &via).unwrap();
    let state = tmp.path().join("state");
    // the directory given is a link: it is resolved first, and everything below it is inside it
    let r = import_pc(&pc_args(&via, &state), 1).unwrap();
    assert!(
        r.warnings.iter().all(|w| !w.contains("symbolic link")),
        "{:?}",
        r.warnings
    );
    assert!(r
        .files
        .iter()
        .any(|f| f.logical_path == "data/field/flevel.lgp"));

    // the check made again, on the real path, right before a file is read: a regular file the
    // scan saw that was replaced by a link out of the tree in the meantime is refused
    let canonical = fs::canonicalize(&real).unwrap();
    let outside = tmp.path().join("elsewhere");
    touch(&outside, "secret.txt", SECRET);
    assert!(super::pc::resolve_in_install(&canonical, "data/field/flevel.lgp").is_ok());
    fs::remove_file(real.join("data/field/flevel.lgp")).unwrap();
    symlink(
        outside.join("secret.txt"),
        real.join("data/field/flevel.lgp"),
    )
    .unwrap();
    let err = super::pc::resolve_in_install(&canonical, "data/field/flevel.lgp").unwrap_err();
    assert!(
        matches!(&err, CommandError::SymlinkOutsideInstall { path, .. } if path == "data/field/flevel.lgp"),
        "{err}"
    );
    assert!(err.to_string().contains("refusing to follow"), "{err}");
    // and a path that does not exist is the usual scan I/O error, not a panic
    assert!(super::pc::resolve_in_install(&canonical, "data/nothing.lgp").is_err());
}

/// Everything that can refuse an import from the install alone happens before the library is
/// opened: a file that changes between the scan and the import's own checks (swapped for a link
/// out of the tree, or gone) is refused on a FRESH state directory without creating it.
#[cfg(unix)]
#[test]
fn a_refusal_from_the_installs_own_checks_creates_no_library() {
    use super::pc::seam;
    use std::os::unix::fs::symlink;
    let tmp = tempfile::tempdir().unwrap();
    let install = tmp.path().join("FF7");
    let outside = tmp.path().join("elsewhere");
    small_install(&install);
    touch(&install, "data/field/flevel.lgp", b"the real field archive");
    touch(&outside, "secret.txt", SECRET);
    let state = tmp.path().join("state");

    // the file is swapped for a link out of the tree after the scan has accepted it
    let (to, swap_in) = (outside.join("secret.txt"), install.join("data/field/flevel.lgp"));
    seam::AFTER_SCAN.with(|h| {
        *h.borrow_mut() = Some(Box::new(move |_dir: &Path| {
            fs::remove_file(&swap_in).unwrap();
            symlink(&to, &swap_in).unwrap();
        }));
    });
    let r = import_pc(&pc_args(&install, &state), 1);
    seam::AFTER_SCAN.with(|h| *h.borrow_mut() = None);
    let err = r.unwrap_err();
    assert!(matches!(err, CommandError::SymlinkOutsideInstall { .. }), "{err}");
    assert!(!state.exists(), "no library, no lock, no .gitignore: {state:?}");

    // the same for a file that vanished (the stamp cannot be taken)
    fs::remove_file(install.join("data/field/flevel.lgp")).unwrap();
    touch(&install, "data/field/flevel.lgp", b"the real field archive");
    let gone = install.join("data/battle/scene.bin");
    seam::AFTER_SCAN.with(|h| {
        *h.borrow_mut() = Some(Box::new(move |_dir: &Path| fs::remove_file(&gone).unwrap()));
    });
    let r = import_pc(&pc_args(&install, &state), 2);
    seam::AFTER_SCAN.with(|h| *h.borrow_mut() = None);
    assert!(r.is_err());
    assert!(!state.exists());

    // and with nothing changed in between the same import goes through
    touch(&install, "data/battle/scene.bin", b"scene bytes");
    import_pc(&pc_args(&install, &state), 3).unwrap();
    assert!(state.join("library").is_dir());
}

/// The audit's own probe, as a test: a link named like the field archive, aimed at a secret.
#[cfg(unix)]
#[test]
fn the_audits_symlink_probe_no_longer_imports_the_secret() {
    use std::os::unix::fs::symlink;
    let tmp = tempfile::tempdir().unwrap();
    let secret_dir = tmp.path().join("fake-home/.ssh");
    fs::create_dir_all(&secret_dir).unwrap();
    let secret = secret_dir.join("id_fake");
    fs::write(&secret, SECRET).unwrap();
    let install = tmp.path().join("install");
    fs::create_dir_all(install.join("data/field")).unwrap();
    fs::write(install.join("ff7.exe"), b"fake exe bytes").unwrap();
    symlink(&secret, install.join("data/field/flevel.lgp")).unwrap();
    let state = tmp.path().join("state");
    let err = import_pc(&pc_args(&install, &state), 1).unwrap_err();
    assert!(
        matches!(err, CommandError::NoFlevelOnlyLinks { .. }),
        "{err}"
    );
    assert!(!state.exists());
}

// ---------------------------------------------------------------------------------------------
// finding 8: disc images that are links
// ---------------------------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn a_disc_image_may_be_a_link_and_the_image_it_leads_to_is_what_is_imported_and_guarded() {
    use std::os::unix::fs::symlink;
    let tmp = tempfile::tempdir().unwrap();
    let store = tmp.path().join("store");
    let links = tmp.path().join("links");
    fs::create_dir_all(&links).unwrap();
    let cue = write_disc(&store, "d1", &disc_iso("SCUS_941.63", "one"));
    // the cue sheet and its bin are both linked in
    symlink(&cue, links.join("d1.cue")).unwrap();
    symlink(store.join("d1.bin"), links.join("d1.bin")).unwrap();
    let state = tmp.path().join("state");
    let r = import_disc(&disc_args(&links.join("d1.cue"), &state, "ff7"), 1).unwrap();
    assert_eq!(r.totals.files, 4);
    assert_eq!(
        r.source_evidence.files_checked, 2,
        "the real .bin and the real .cue"
    );
    // the same image, linked, imported again: the same disc, nothing new
    let again = import_disc(&disc_args(&links.join("d1.cue"), &state, "ff7"), 2).unwrap();
    assert_eq!(again.totals.entries_added, 0);
    // a plain .iso behind a link works too
    let iso = tmp.path().join("store/other.iso");
    fs::write(&iso, disc_iso("SCUS_941.64", "two")).unwrap();
    symlink(&iso, links.join("other.iso")).unwrap();
    let r2 = import_disc(&disc_args(&links.join("other.iso"), &state, "ff7"), 3).unwrap();
    assert_eq!(r2.disc.index, 2);
    // the originals were not touched
    assert_eq!(fs::read(&iso).unwrap(), disc_iso("SCUS_941.64", "two"));
    assert!(fs::symlink_metadata(links.join("d1.cue"))
        .unwrap()
        .file_type()
        .is_symlink());

    // a link that points INTO the library is refused like the file itself, with nothing created
    let lib_file = state.join("library").join("sneaky.iso");
    fs::write(&lib_file, disc_iso("SCUS_941.65", "three")).unwrap();
    symlink(&lib_file, links.join("into-library.iso")).unwrap();
    let before = snapshot(&state);
    let err = import_disc(
        &disc_args(&links.join("into-library.iso"), &state, "ff7"),
        4,
    )
    .unwrap_err();
    assert!(
        matches!(
            err,
            CommandError::Library(LibraryError::SourceInsideLibrary(_))
        ),
        "{err}"
    );
    assert_eq!(snapshot(&state), before);
}

// ---------------------------------------------------------------------------------------------
// finding 9: a crafted disc that claims a genuine serial
// ---------------------------------------------------------------------------------------------

/// A disc (non-synthetic label) of serial `exe` holding `files` plus its SYSTEM.CNF.
fn disc_with(exe: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
    let cnf = system_cnf(exe);
    let mut all: Vec<(&str, &[u8])> = vec![("SYSTEM.CNF", cnf.as_slice())];
    all.extend_from_slice(files);
    build_iso_labeled(DISC_LABEL, &all)
}

#[test]
fn a_crafted_disc_claiming_a_genuine_serial_is_told_apart_and_cannot_poison_the_set() {
    let tmp = tempfile::tempdir().unwrap();
    let images = tmp.path().join("images");
    let state = tmp.path().join("state");
    // the crafted image comes first: it claims disc 1 of the table's NTSC-U set
    let hostile = write_disc(
        &images,
        "hostile",
        &disc_with(
            "SCUS_941.63",
            &[
                ("FIELD/X.DAT", b"hostile bytes"),
                ("EXTRA/Y.DAT", b"more of them"),
            ],
        ),
    );
    let first = import_disc(&disc_args(&hostile, &state, "evil"), 1).unwrap();
    assert_eq!(first.disc.index, 1);
    // the genuine disc 1 (another session: the table's family is shared by every session)
    let genuine_iso = disc_with(
        "SCUS_941.63",
        &[
            ("FIELD/X.DAT", b"genuine bytes"),
            ("FIELD/MD1STIN.DAT", TEST_PAYLOAD_A),
        ],
    );
    let genuine = write_disc(&images, "genuine", &genuine_iso);
    let manifest_before = fs::read(manifest_file(&state, &first.set_id)).unwrap();
    let blobs_before = count_files(&state.join("library/blobs"));

    let err = import_disc(&disc_args(&genuine, &state, "ff7"), 2).unwrap_err();
    // a clear "different disc", not a confusing EntryConflict
    assert!(
        matches!(err, CommandError::DiscContentDiffers { index: 1, .. }),
        "{err}"
    );
    let msg = err.to_string();
    assert!(
        msg.contains("different disc content for SCUS-94163 disc 1"),
        "{msg}"
    );
    assert!(
        msg.contains("--replace-disc"),
        "the way out is named: {msg}"
    );
    assert!(
        !msg.contains("replacing entries is not supported yet"),
        "{msg}"
    );
    // ... and the refusal changed nothing that matters: manifest, blobs, sessions
    assert_eq!(
        fs::read(manifest_file(&state, &first.set_id)).unwrap(),
        manifest_before
    );
    assert_eq!(
        count_files(&state.join("library/blobs")),
        blobs_before,
        "not one blob for the refused disc"
    );
    assert_eq!(count_files(&state.join("library/tmp")), 0);
    assert!(
        load_session(&state, "ff7").unwrap().is_none(),
        "no session record for the refused disc"
    );
    assert!(load_session(&state, "evil").unwrap().unwrap().discs[0].complete);

    // the genuine disc takes its number back explicitly ...
    let mut replace = disc_args(&genuine, &state, "ff7");
    replace.replace_disc = true;
    let r = import_disc(&replace, 3).unwrap();
    let replaced = r.replaced.as_ref().expect("a disc was replaced");
    assert_eq!(replaced.serial.as_deref(), Some("SCUS-94163"));
    assert_eq!(
        replaced.entries, 3,
        "SYSTEM.CNF, FIELD/X.DAT and EXTRA/Y.DAT of the crafted image"
    );
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("--replace-disc") && w.contains("replaced")),
        "{:?}",
        r.warnings
    );
    let m = manifest_of(&state, &r.set_id);
    let mut paths: Vec<&str> = m.entries.iter().map(|e| e.logical_path.as_str()).collect();
    paths.sort_unstable();
    assert_eq!(
        paths,
        [
            "disc1/FIELD/MD1STIN.DAT",
            "disc1/FIELD/X.DAT",
            "disc1/SYSTEM.CNF"
        ],
        "nothing of the crafted disc is left in the set"
    );
    assert_eq!(m.discs.len(), 1);
    assert_eq!(m.discs[0], r.content);
    assert_eq!(
        m.provenance.sources,
        vec![SourceRef::Disc {
            index: 1,
            serial: "SCUS-94163".into()
        }]
    );
    let lib = Library::open_existing(&state).unwrap();
    assert!(lib.verify_set(&r.set_id).unwrap().is_empty());
    // the old blobs stay in the store (blobs are shared and content-addressed)
    assert!(count_files(&state.join("library/blobs")) > blobs_before);
    // ... and now it is the CRAFTED image that is the odd one out, the other way round
    let err = import_disc(&disc_args(&hostile, &state, "evil"), 4).unwrap_err();
    assert!(
        matches!(err, CommandError::DiscContentDiffers { index: 1, .. }),
        "{err}"
    );
    // the genuine image again is simply confirmed: nothing is rewritten
    let manifest_now = fs::read(manifest_file(&state, &r.set_id)).unwrap();
    let again = import_disc(&disc_args(&genuine, &state, "ff7"), 5).unwrap();
    assert_eq!(again.totals.entries_added, 0);
    assert!(again.replaced.is_none());
    assert_eq!(
        fs::read(manifest_file(&state, &r.set_id)).unwrap(),
        manifest_now
    );
}

/// What the library holds is only ever CONFIRMED: an image with a new file, with other bytes or
/// with a file fewer is a different disc, whichever way it differs.
#[test]
fn an_image_can_only_confirm_a_disc_that_is_recorded_not_grow_or_shrink_it() {
    let tmp = tempfile::tempdir().unwrap();
    let images = tmp.path().join("images");
    let state = tmp.path().join("state");
    const ONE: &[u8] = b"one";
    const TWO: &[u8] = b"two";
    const TWO_CHANGED: &[u8] = b"TWO!";
    const THREE: &[u8] = b"three";
    const FOUR: &[u8] = b"four";
    let base: &[(&str, &[u8])] = &[
        ("A/ONE.DAT", ONE),
        ("A/TWO.DAT", TWO),
        ("B/THREE.DAT", THREE),
    ];
    let first = import_disc(
        &disc_args(
            &write_disc(&images, "base", &disc_with("SCUS_941.64", base)),
            &state,
            "s",
        ),
        1,
    )
    .unwrap();
    assert_eq!(first.disc.index, 2);
    assert_eq!(first.content.files, 4);
    assert_eq!(first.content.serial, "SCUS-94164");
    let manifest_before = fs::read(manifest_file(&state, &first.set_id)).unwrap();
    let session_before = fs::read(state.join("import-sessions/s.json")).unwrap();

    type Files<'a> = Vec<(&'a str, &'a [u8])>;
    let mut with_four: Files = base.to_vec();
    with_four.push(("B/FOUR.DAT", FOUR));
    let changed: Files = vec![
        ("A/ONE.DAT", ONE),
        ("A/TWO.DAT", TWO_CHANGED),
        ("B/THREE.DAT", THREE),
    ];
    let fewer: Files = vec![("A/ONE.DAT", ONE), ("A/TWO.DAT", TWO)];
    let variants: [(&str, Files, &str); 3] = [
        ("a new file", with_four, "FOUR.DAT"),
        ("other bytes", changed, "TWO.DAT"),
        ("a file fewer", fewer, "missing from this image"),
    ];
    for (what, files, expect) in variants {
        let img = write_disc(
            &images,
            &format!("v-{}", what.replace(' ', "-")),
            &disc_with("SCUS_941.64", &files),
        );
        let err = import_disc(&disc_args(&img, &state, "s"), 2).unwrap_err();
        assert!(
            matches!(err, CommandError::DiscContentDiffers { index: 2, .. }),
            "{what}: {err}"
        );
        assert!(err.to_string().contains(expect), "{what}: {err}");
        assert_eq!(
            fs::read(manifest_file(&state, &first.set_id)).unwrap(),
            manifest_before,
            "{what}"
        );
        assert_eq!(
            fs::read(state.join("import-sessions/s.json")).unwrap(),
            session_before,
            "{what}"
        );
        assert_eq!(count_files(&state.join("library/tmp")), 0, "{what}");
    }
    // the genuine image is still confirmed, and an identical second rip too
    let same = write_disc(&images, "same-again", &disc_with("SCUS_941.64", base));
    let r = import_disc(&disc_args(&same, &state, "s"), 3).unwrap();
    assert_eq!((r.totals.entries_added, r.totals.entries_unchanged), (0, 4));
    assert_eq!(
        fs::read(manifest_file(&state, &first.set_id)).unwrap(),
        manifest_before
    );
}

/// The advisor's case: XA stream files are never stored, so a disc full of them must still be
/// confirmed by another session without a complaint about "new" files.
#[test]
fn a_disc_with_xa_streams_is_confirmed_by_a_second_import_of_the_same_image() {
    let tmp = tempfile::tempdir().unwrap();
    let images = tmp.path().join("images");
    let state = tmp.path().join("state");
    let iso = disc_with(
        "SCUS_941.63",
        &[
            ("FIELD/MD1STIN.DAT", TEST_PAYLOAD_A),
            ("MOVIE/OPENING.MOV", b"xa stream bytes"),
            ("MOVIE/ENDING.MOV", b"more xa"),
        ],
    );
    let mut raw = wrap_raw(&iso, RawMode::Mode2Form1);
    for movie in ["MOVIE/OPENING.MOV", "MOVIE/ENDING.MOV"] {
        let lba = crate::synthetic::lba_of(&iso, movie).unwrap() as usize;
        mark_form2(&mut raw, lba);
    }
    let cue = write_raw(&images, "xa", &raw);
    let first = import_disc(&disc_args(&cue, &state, "one"), 1).unwrap();
    assert_eq!(first.skipped_xa_count, 2);
    assert_eq!(first.totals.files, 2, "SYSTEM.CNF and MD1STIN.DAT");
    let manifest = fs::read(manifest_file(&state, &first.set_id)).unwrap();
    // the same image under another session: streams are skipped again, everything else matches
    let second = import_disc(&disc_args(&cue, &state, "two"), 2).unwrap();
    assert_eq!(second.skipped_xa_count, 2);
    assert_eq!(
        (second.totals.entries_added, second.totals.entries_unchanged),
        (0, 2)
    );
    assert_eq!(second.content, first.content);
    assert_eq!(
        fs::read(manifest_file(&state, &first.set_id)).unwrap(),
        manifest,
        "the set is not even rewritten"
    );
    // a plain 2048-byte rip of that disc cannot tell streams from files, so it holds files the
    // raw rip did not store: that is another image of the disc, and `--replace-disc` is the way
    let cooked = write_disc_cooked(&images, "xa-cooked", &iso);
    let err = import_disc(&disc_args(&cooked, &state, "two"), 3).unwrap_err();
    assert!(
        matches!(err, CommandError::DiscContentDiffers { .. }),
        "{err}"
    );
    assert!(err.to_string().contains("imported another way"), "{err}");
    let mut replace = disc_args(&cooked, &state, "two");
    replace.replace_disc = true;
    let r = import_disc(&replace, 4).unwrap();
    assert_eq!(
        r.totals.files, 4,
        "the cooked image has the streams as ordinary files"
    );
}

fn write_disc_cooked(dir: &Path, name: &str, iso: &[u8]) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let p = dir.join(format!("{name}.iso"));
    fs::write(&p, iso).unwrap();
    p
}

/// A manifest written before disc records existed has no digest to compare with: the library's
/// own conflict check still refuses other content (now in plain words), and the first import
/// that confirms the disc records it.
#[test]
fn a_set_without_disc_records_still_refuses_other_content_and_gets_its_record_on_the_next_import() {
    let tmp = tempfile::tempdir().unwrap();
    let images = tmp.path().join("images");
    let state = tmp.path().join("state");
    let genuine = write_disc(&images, "g", &disc_iso("SCUS_941.63", "genuine"));
    let first = import_disc(&disc_args(&genuine, &state, "a"), 1).unwrap();
    strip_disc_records(&state, &first.set_id);
    assert!(
        manifest_of(&state, &first.set_id).discs.is_empty(),
        "an old-style manifest"
    );

    let other = write_disc(&images, "o", &disc_iso("SCUS_941.63", "someone else's"));
    let err = import_disc(&disc_args(&other, &state, "b"), 2).unwrap_err();
    assert!(
        matches!(&err, CommandError::DiscContentDiffers { detail, .. } if detail.contains("DISC.TXT") && detail.contains("other bytes")),
        "{err}"
    );
    assert!(!matches!(err, CommandError::Library(_)), "{err}");

    // the genuine image again: accepted, and now the set records what the disc held
    let again = import_disc(&disc_args(&genuine, &state, "b"), 3).unwrap();
    assert_eq!(again.totals.entries_added, 0);
    let m = manifest_of(&state, &first.set_id);
    assert_eq!(m.discs, vec![first.content.clone()]);
}

#[test]
fn replace_disc_replaces_one_disc_and_a_failed_replacement_changes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let images = tmp.path().join("images");
    let state = tmp.path().join("state");
    let d1 = write_disc(&images, "d1", &disc_iso("SCUS_941.63", "one"));
    let d2 = write_disc(&images, "d2", &disc_iso("SCUS_941.64", "two"));
    let d3 = write_disc(&images, "d3", &disc_iso("SCUS_941.65", "three"));
    for (i, d) in [&d1, &d2, &d3].into_iter().enumerate() {
        import_disc(&disc_args(d, &state, "ff7"), i as u64 + 1).unwrap();
    }
    let set_id = import_status("ff7", Some(&state)).unwrap().set_id;
    let before = manifest_of(&state, &set_id);
    assert_eq!(before.discs.len(), 3);

    // a replacement that cannot be read to the end: the old disc 2 is exactly as it was
    let broken = big_disc(&images, "broken2", "x2", "SCUS_941.64", 3, true);
    let mut replace = disc_args(&broken, &state, "ff7");
    replace.replace_disc = true;
    let manifest_bytes = fs::read(manifest_file(&state, &set_id)).unwrap();
    let session_bytes = fs::read(state.join("import-sessions/ff7.json")).unwrap();
    assert!(import_disc(&replace, 10).is_err());
    assert_eq!(
        fs::read(manifest_file(&state, &set_id)).unwrap(),
        manifest_bytes
    );
    assert_eq!(
        fs::read(state.join("import-sessions/ff7.json")).unwrap(),
        session_bytes
    );
    assert_eq!(count_files(&state.join("library/tmp")), 0);

    // a good replacement of disc 2 leaves discs 1 and 3 alone
    let good2 = big_disc(&images, "good2", "x2", "SCUS_941.64", 3, false);
    let mut replace = disc_args(&good2, &state, "ff7");
    replace.replace_disc = true;
    let r = import_disc(&replace, 11).unwrap();
    assert_eq!(r.disc.index, 2);
    assert_eq!(r.replaced.as_ref().map(|d| d.entries), Some(4));
    assert_eq!(
        r.totals.entries_added, 5,
        "SYSTEM.CNF, F000-F002 and ZZ/LAST.DAT"
    );
    let after = manifest_of(&state, &set_id);
    for keep in [1u8, 3] {
        assert_eq!(
            after.disc(keep),
            before.disc(keep),
            "disc {keep} was not touched"
        );
        let prefix = format!("disc{keep}/");
        let pick = |m: &fmv_library::Manifest| {
            m.entries
                .iter()
                .filter(|e| e.logical_path.starts_with(&prefix))
                .cloned()
                .collect::<Vec<_>>()
        };
        assert_eq!(pick(&after), pick(&before));
    }
    assert_ne!(after.disc(2), before.disc(2));
    assert_eq!(after.disc(2), Some(&r.content));
    assert!(import_status("ff7", Some(&state)).unwrap().complete);

    // with nothing recorded under the number it is an ordinary import, with a note
    let mut fresh_state = disc_args(&d1, &tmp.path().join("state-b"), "ff7");
    fresh_state.replace_disc = true;
    let r = import_disc(&fresh_state, 1).unwrap();
    assert!(r.replaced.is_none());
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("nothing was recorded as disc 1")),
        "{:?}",
        r.warnings
    );
}

#[test]
fn replace_disc_can_take_a_number_over_from_another_serial_but_not_split_a_disc() {
    let tmp = tempfile::tempdir().unwrap();
    let images = tmp.path().join("images");
    let state = tmp.path().join("state");
    let a = write_disc(&images, "a", &disc_iso("SLPS_914.40", "disc one"));
    let b = write_disc(&images, "b", &disc_iso("SLPS_914.41", "another disc"));
    let mut o = disc_args(&a, &state, "uh");
    o.disc = Some(1);
    import_disc(&o, 1).unwrap();
    let mut other = disc_args(&b, &state, "uh");
    other.disc = Some(1);
    // without the flag: the number is taken (unchanged behaviour)
    assert!(matches!(
        import_disc(&other, 2),
        Err(CommandError::DiscIndexTaken { .. })
    ));
    // with it: this image is disc 1 now, in the session and in the set
    other.replace_disc = true;
    let r = import_disc(&other, 3).unwrap();
    assert_eq!(r.disc.index, 1);
    assert_eq!(
        r.replaced
            .as_ref()
            .and_then(|d| d.serial.clone())
            .as_deref(),
        Some("SLPS-91440")
    );
    let st = import_status("uh", Some(&state)).unwrap();
    assert_eq!(st.imported.len(), 1);
    assert_eq!(st.imported[0].serial, "SLPS-91441");
    let m = manifest_of(&state, &r.set_id);
    assert_eq!(
        m.provenance.sources,
        vec![SourceRef::Disc {
            index: 1,
            serial: "SLPS-91441".into()
        }]
    );
    assert!(m
        .entries
        .iter()
        .all(|e| e.logical_path.starts_with("disc1/")));
    // but the same serial under a SECOND number is still refused, flag or not
    let mut twice = disc_args(&b, &state, "uh");
    twice.disc = Some(2);
    twice.replace_disc = true;
    assert!(matches!(
        import_disc(&twice, 4),
        Err(CommandError::SerialAlreadyImported { .. })
    ));
}

// ---------------------------------------------------------------------------------------------
// finding 10: private state, no traces, locks, leftovers
// ---------------------------------------------------------------------------------------------

#[cfg(unix)]
fn assert_private(root: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mode = |p: &Path| fs::symlink_metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(root) & 0o077, 0, "{root:?} is {:o}", mode(root));
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap().flatten() {
            let m = mode(&e.path());
            assert_eq!(
                m & 0o077,
                0,
                "{:?} is {m:o}: group/world accessible",
                e.path()
            );
            if e.file_type().unwrap().is_dir() {
                stack.push(e.path());
            }
        }
    }
}

/// The audit's `perms` probe: after import-disc on a fresh state directory `<state>`,
/// `import-sessions/` and its lock file used to be 0755/0755/0644.
#[cfg(unix)]
#[test]
fn everything_an_import_creates_is_private() {
    let tmp = tempfile::tempdir().unwrap();
    let images = tmp.path().join("images");
    let disc = write_disc(&images, "d1", &disc_iso("SCUS_941.63", "one"));
    let state = tmp.path().join("deeper").join("state");
    import_disc(&disc_args(&disc, &state, "ff7"), 1).unwrap();
    assert_private(&state);
    assert_private(&tmp.path().join("deeper"));
    assert!(state.join("import-sessions/.ff7.lock").is_file());
    assert!(state.join("import-sessions/ff7.json").is_file());
    // and so does a PC import
    let install = tmp.path().join("FF7");
    fake_install(&install);
    let state2 = tmp.path().join("other").join("state");
    import_pc(&pc_args(&install, &state2), 1).unwrap();
    assert_private(&state2);
}

/// An image inside the library is refused before a lock, a directory or a file exists for it
/// (the audit's `perms` probe found the session lock, `library/` and `.gitignore` left behind).
#[test]
fn an_image_inside_the_library_is_refused_without_a_trace() {
    let tmp = tempfile::tempdir().unwrap();
    let images = tmp.path().join("images");
    let state = tmp.path().join("state");
    let good = write_disc(&images, "d1", &disc_iso("SCUS_941.63", "one"));
    import_disc(&disc_args(&good, &state, "ff7"), 1).unwrap();
    // an image planted inside the library (the planting is part of the "before" picture)
    let cue = write_disc(
        &state.join("library"),
        "evil",
        &disc_iso("SCUS_941.64", "evil"),
    );
    let before = snapshot(&state);
    let err = import_disc(&disc_args(&cue, &state, "newsession"), 2).unwrap_err();
    assert!(
        matches!(
            err,
            CommandError::Library(LibraryError::SourceInsideLibrary(_))
        ),
        "{err}"
    );
    assert_eq!(
        snapshot(&state),
        before,
        "not a lock, a directory or a file was created for the refusal"
    );
    assert!(!state.join("import-sessions/.newsession.lock").exists());
    assert!(load_session(&state, "newsession").unwrap().is_none());

    // the audit's exact probe: a FRESH state directory whose library holds nothing but the image
    let state2 = tmp.path().join("state2");
    let evil2 = write_disc(
        &state2.join("library"),
        "evil",
        &disc_iso("SCUS_941.64", "evil"),
    );
    let before2 = snapshot(&state2);
    assert!(matches!(
        import_disc(&disc_args(&evil2, &state2, "x"), 3),
        Err(CommandError::Library(LibraryError::SourceInsideLibrary(_)))
    ));
    assert_eq!(
        snapshot(&state2),
        before2,
        "no import-sessions/, no lock, no library skeleton, no .gitignore"
    );
    assert!(!state2.join("import-sessions").exists());
    assert!(!state2.join("library/blobs").exists() && !state2.join("library/.gitignore").exists());
}

#[test]
fn a_session_import_that_is_refused_before_the_lock_creates_nothing_even_in_a_new_state_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let images = tmp.path().join("images");
    let state = tmp.path().join("state");
    // a perfectly good image, a state directory that does not exist, and a refusal that needs
    // no lock: a bad disc number
    let d = write_disc(&images, "d1", &disc_iso("SCUS_941.63", "one"));
    let mut args = disc_args(&d, &state, "ff7");
    args.disc = Some(2); // the table says this serial is disc 1
    assert!(matches!(
        import_disc(&args, 1),
        Err(CommandError::DiscContradiction { .. })
    ));
    assert!(!state.exists(), "nothing was created for a refusal");
    // an image that is not a disc at all
    let junk = tmp.path().join("junk.iso");
    fs::write(&junk, vec![3u8; 50_000]).unwrap();
    assert!(import_disc(&disc_args(&junk, &state, "ff7"), 2).is_err());
    assert!(!state.exists());
}

#[test]
fn killed_writers_leave_temp_files_that_the_next_import_sweeps() {
    let tmp = tempfile::tempdir().unwrap();
    let images = tmp.path().join("images");
    let state = tmp.path().join("state");
    let d1 = write_disc(&images, "d1", &disc_iso("SCUS_941.63", "one"));
    let d2 = write_disc(&images, "d2", &disc_iso("SCUS_941.64", "two"));
    let first = import_disc(&disc_args(&d1, &state, "ff7"), 1).unwrap();
    let set_dir = state.join("library/sets").join(&first.set_id);
    let sessions = state.join("import-sessions");
    let mk = |dir: &Path, name: &str| {
        let p = dir.join(name);
        fs::write(&p, vec![1u8; 4096]).unwrap();
        backdate(&p, 3600);
        p
    };
    let manifest_tmp = mk(&set_dir, ".manifest.json.tmp-4242-0-deadbeef");
    let session_tmp = mk(&sessions, ".ff7.json.tmp-4242-1-deadbeef");
    let other_session_tmp = mk(&sessions, ".other.json.tmp-4242-2-deadbeef");
    let young = set_dir.join(".manifest.json.tmp-4242-3-cafef00d");
    fs::write(&young, b"a writer that has just started").unwrap();
    // dated ahead, so that no slow machine can age it past the sweep threshold
    fs::OpenOptions::new()
        .write(true)
        .open(&young)
        .unwrap()
        .set_modified(SystemTime::now() + Duration::from_secs(3600))
        .unwrap();

    let r = import_disc(&disc_args(&d2, &state, "ff7"), 2).unwrap();
    assert!(
        !manifest_tmp.exists(),
        "the half-written manifest of a killed writer is gone"
    );
    assert!(!session_tmp.exists(), "so is the half-written session file");
    assert!(
        other_session_tmp.exists(),
        "another session's leftovers are not this session's to sweep"
    );
    assert!(
        young.exists(),
        "a young temp file may belong to a live writer"
    );
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("leftover temporary file")),
        "{:?}",
        r.warnings
    );
    // nothing else changed: the session and the set are intact
    assert!(import_status("ff7", Some(&state)).unwrap().imported.len() == 2);
    assert!(Library::open_existing(&state)
        .unwrap()
        .verify_set(&first.set_id)
        .unwrap()
        .is_empty());
}

#[test]
fn imports_go_ahead_unlocked_where_the_filesystem_has_no_locks_and_say_so() {
    let tmp = tempfile::tempdir().unwrap();
    let images = tmp.path().join("images");
    let disc = write_disc(&images, "d1", &disc_iso("SCUS_941.63", "one"));
    let install = tmp.path().join("FF7");
    fake_install(&install);

    let normal = import_disc(&disc_args(&disc, &tmp.path().join("s0"), "ff7"), 1).unwrap();
    assert!(
        normal
            .warnings
            .iter()
            .all(|w| !w.contains("advisory file locks")),
        "{:?}",
        normal.warnings
    );

    let _no_locks = NoLocks::on();
    let r = import_disc(&disc_args(&disc, &tmp.path().join("s1"), "ff7"), 1).unwrap();
    assert!(
        r.warnings.iter().any(|w| w.contains("advisory file locks")),
        "{:?}",
        r.warnings
    );
    assert_eq!(r.totals.files, 4, "the import itself is unaffected");
    let p = import_pc(&pc_args(&install, &tmp.path().join("s2")), 1).unwrap();
    assert!(
        p.warnings.iter().any(|w| w.contains("advisory file locks")),
        "{:?}",
        p.warnings
    );
    // a session lock on such a filesystem is "taken" without being held, and says so
    let lock = lock_session(&tmp.path().join("s3"), "ff7").unwrap();
    assert!(!lock.is_held());
    // and with locks available again the same call really holds the lock
    drop(lock);
    drop(_no_locks);
    let held = lock_session(&tmp.path().join("s3"), "ff7").unwrap();
    assert!(held.is_held());
    assert!(matches!(
        lock_session(&tmp.path().join("s3"), "ff7"),
        Err(SessionError::Busy(_))
    ));
}

// ---------------------------------------------------------------------------------------------
// finding 7: remove-set
// ---------------------------------------------------------------------------------------------

fn remove_args(set_id: &str, state: &Path, yes: bool) -> RemoveSetArgs {
    RemoveSetArgs {
        set_id: set_id.to_string(),
        state_dir: Some(state.to_path_buf()),
        confirmed: yes,
    }
}

#[test]
fn remove_set_needs_confirmation_and_then_drops_the_set_and_its_sessions_only() {
    let tmp = tempfile::tempdir().unwrap();
    let images = tmp.path().join("images");
    let state = tmp.path().join("state");
    let install = tmp.path().join("FF7");
    fake_install(&install);
    let d1 = write_disc(&images, "d1", &disc_iso("SCUS_941.63", "one"));
    let d2 = write_disc(&images, "d2", &disc_iso("SCUS_941.64", "two"));
    let u1 = write_disc(&images, "u1", &disc_iso("SLPS_914.40", "unknown disc"));
    let pc = import_pc(&pc_args(&install, &state), 1).unwrap();
    let ps1 = import_disc(&disc_args(&d1, &state, "ff7"), 2).unwrap();
    import_disc(&disc_args(&d2, &state, "ff7"), 3).unwrap();
    import_disc(&disc_args(&d1, &state, "second-session"), 4).unwrap();
    import_disc(&disc_args(&u1, &state, "mystery"), 5).unwrap();
    let originals_before = (snapshot(&install), snapshot(&images));
    let blobs_before = count_files(&state.join("library/blobs"));
    assert_eq!(set_ids(&state).len(), 3);

    // without --yes: nothing changes, and the message says what would go
    let before = snapshot(&state);
    let err = remove_set(&remove_args(&ps1.set_id, &state, false)).unwrap_err();
    assert!(
        matches!(err, CommandError::ConfirmationRequired(_)),
        "{err}"
    );
    let msg = err.to_string();
    assert!(
        msg.contains(&ps1.set_id) && msg.contains("--yes") && msg.contains("8 files"),
        "{msg}"
    );
    assert!(
        msg.contains("ff7") && msg.contains("second-session") && !msg.contains("mystery"),
        "{msg}"
    );
    assert_eq!(snapshot(&state), before);

    // with it: the set and the two sessions that point at it are gone, nothing else is
    let r = remove_set(&remove_args(&ps1.set_id, &state, true)).unwrap();
    assert_eq!(
        (r.entries, r.bytes.is_some(), r.manifest_problem.is_none()),
        (Some(8), true, true)
    );
    assert_eq!(r.sessions_removed, ["ff7", "second-session"]);
    assert!(r.blobs.starts_with("kept") && r.originals.starts_with("untouched"));
    assert!(matches!(
        import_status("ff7", Some(&state)),
        Err(CommandError::NoSuchSession(_))
    ));
    assert!(
        import_status("mystery", Some(&state)).is_ok(),
        "an unrelated session stays"
    );
    let left = set_ids(&state);
    assert_eq!(left.len(), 2);
    assert!(left.contains(&pc.set_id) && !left.contains(&ps1.set_id));
    assert_eq!(
        count_files(&state.join("library/blobs")),
        blobs_before,
        "blobs are kept"
    );
    assert!(Library::open_existing(&state)
        .unwrap()
        .verify_set(&pc.set_id)
        .unwrap()
        .is_empty());
    assert_eq!(
        (snapshot(&install), snapshot(&images)),
        originals_before,
        "originals untouched"
    );
    // lock files of the sessions are never deleted
    assert!(state.join("import-sessions/.ff7.lock").exists());

    // the set can be imported afresh, reusing the blobs that were kept
    let again = import_disc(&disc_args(&d1, &state, "ff7"), 6).unwrap();
    assert_eq!(again.totals.blobs_created, 0);
    assert_eq!(again.totals.entries_added, 4);
    // removing something that is not there, or not an id, says so
    assert!(matches!(
        remove_set(&remove_args("no-such-set-00000000", &state, true)),
        Err(CommandError::Library(LibraryError::SetNotFound(_)))
    ));
    assert!(matches!(
        remove_set(&remove_args("../library", &state, true)),
        Err(CommandError::Library(LibraryError::BadSetId(_)))
    ));
}

/// Finding 7: the set the audit bricked (a manifest over the reader's cap) can be dropped, and
/// so can one whose manifest is garbage or missing.
#[test]
fn a_set_that_cannot_be_read_any_more_can_still_be_removed() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let install = tmp.path().join("FF7");
    fake_install(&install);
    let pc = import_pc(&pc_args(&install, &state), 1).unwrap();
    let path = manifest_file(&state, &pc.set_id);
    // the brick: a manifest one byte over the cap
    let f = fs::OpenOptions::new().write(true).open(&path).unwrap();
    f.set_len(fmv_library::MAX_MANIFEST_BYTES + 1).unwrap();
    drop(f);
    let lst = list_sets(Some(&state), false).unwrap();
    assert!(
        lst.sets[0].error.as_deref().unwrap().contains("too large"),
        "list shows the problem"
    );
    assert!(
        import_pc(&pc_args(&install, &state), 2).is_err(),
        "and the set cannot be imported into"
    );
    let err = remove_set(&remove_args(&pc.set_id, &state, false)).unwrap_err();
    assert!(
        err.to_string().contains("unusable") && err.to_string().contains("too large"),
        "{err}"
    );
    let r = remove_set(&remove_args(&pc.set_id, &state, true)).unwrap();
    assert_eq!(r.entries, None);
    assert!(r.manifest_problem.as_deref().unwrap().contains("too large"));
    assert!(set_ids(&state).is_empty());
    // the install is untouched, and re-importing rebuilds the set (blobs reused)
    let again = import_pc(&pc_args(&install, &state), 3).unwrap();
    assert_eq!(again.totals.blobs_created, 0);
    assert_eq!(again.set_id, pc.set_id);

    // garbage manifest
    fs::write(&path, b"{ not json at all").unwrap();
    let r = remove_set(&remove_args(&pc.set_id, &state, true)).unwrap();
    assert!(r
        .manifest_problem
        .as_deref()
        .unwrap()
        .contains("cannot parse"));
    // a directory without any manifest (what a killed first import leaves)
    let headless = state.join("library/sets/headless-00000000");
    fs::create_dir_all(&headless).unwrap();
    fs::write(headless.join(".manifest.json.tmp-1-0-aaaa"), b"half").unwrap();
    let r = remove_set(&remove_args("headless-00000000", &state, true)).unwrap();
    assert!(r
        .manifest_problem
        .as_deref()
        .unwrap()
        .contains("no manifest"));
    assert!(!headless.exists());
}

#[test]
fn remove_set_refuses_while_the_set_or_a_session_is_in_use_and_changes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let images = tmp.path().join("images");
    let state = tmp.path().join("state");
    let d1 = write_disc(&images, "d1", &disc_iso("SCUS_941.63", "one"));
    let r = import_disc(&disc_args(&d1, &state, "ff7"), 1).unwrap();
    let before = snapshot(&state);

    // an import holds the session
    let hold = lock_session(&state, "ff7").unwrap();
    let err = remove_set(&remove_args(&r.set_id, &state, true)).unwrap_err();
    assert!(
        matches!(err, CommandError::Session(SessionError::Busy(_))),
        "{err}"
    );
    drop(hold);
    assert!(Library::open_existing(&state)
        .unwrap()
        .load_manifest(&r.set_id)
        .is_ok());
    assert!(import_status("ff7", Some(&state)).is_ok());

    // an import holds the set
    let lib = Library::open(&state).unwrap();
    let handle = fmv_library::SetHandle::with_id(
        r.set_id.clone(),
        manifest_of(&state, &r.set_id).provenance.clone(),
    );
    let writer = lib.begin_import(&handle, 2).unwrap();
    let err = remove_set(&remove_args(&r.set_id, &state, true)).unwrap_err();
    assert!(
        matches!(err, CommandError::Library(LibraryError::SetBusy(_))),
        "{err}"
    );
    drop(writer);
    // refusals touched nothing (lock files aside, which exist already)
    let after = snapshot(&state);
    let changed: Vec<&String> = after
        .iter()
        .filter(|(k, v)| before.get(*k) != Some(v) && !k.ends_with(".lock"))
        .map(|(k, _)| k)
        .collect();
    assert!(changed.is_empty(), "{changed:?}");
    assert!(import_status("ff7", Some(&state)).is_ok());
    // and with nothing in the way it goes
    remove_set(&remove_args(&r.set_id, &state, true)).unwrap();
}

// ---------------------------------------------------------------------------------------------
// the audit's `overlap` probe, end to end
// ---------------------------------------------------------------------------------------------

/// 200 records on one shared extent: the import is refused at the walk, nothing is created.
#[test]
fn the_audits_overlap_probe_is_refused_before_anything_is_written() {
    let tmp = tempfile::tempdir().unwrap();
    let images = tmp.path().join("images");
    let state = tmp.path().join("state");
    let payload = vec![7u8; 1024 * 2048];
    let names: Vec<String> = (0..200).map(|i| format!("F{i:05}.BIN")).collect();
    let mut files: Vec<(String, Vec<u8>)> = names
        .iter()
        .map(|n| (n.clone(), payload[..16].to_vec()))
        .collect();
    files.push(("SYSTEM.CNF".into(), system_cnf("SLUS_999.99")));
    files.push(("DATA.BIN".into(), payload.clone()));
    let mut iso = build_iso_labeled(DISC_LABEL, &owned(&files));
    // point every F-record at DATA.BIN's extent, one sector shorter each time
    let data_lba = crate::synthetic::lba_of(&iso, "DATA.BIN").unwrap();
    let root = u32::from_le_bytes(iso[16 * 2048 + 158..16 * 2048 + 162].try_into().unwrap())
        as usize
        * 2048;
    let root_size =
        u32::from_le_bytes(iso[16 * 2048 + 166..16 * 2048 + 170].try_into().unwrap()) as usize;
    let mut sector = 0;
    while sector * 2048 < root_size {
        let base = root + sector * 2048;
        let mut off = 0;
        while off < 2048 && iso[base + off] != 0 {
            let at = base + off;
            let name_len = iso[at + 32] as usize;
            let name = String::from_utf8_lossy(&iso[at + 33..at + 33 + name_len]).into_owned();
            if let Some(i) = name
                .strip_prefix('F')
                .and_then(|r| r.split('.').next())
                .and_then(|r| r.parse::<u32>().ok())
            {
                iso[at + 2..at + 6].copy_from_slice(&data_lba.to_le_bytes());
                iso[at + 10..at + 14].copy_from_slice(&(1024 * 2048 - i).to_le_bytes());
            }
            off += iso[at] as usize;
        }
        sector += 1;
    }
    let image = images.join("amplify.iso");
    fs::create_dir_all(&images).unwrap();
    fs::write(&image, &iso).unwrap();
    let err = import_disc(&disc_args(&image, &state, "amp"), 1).unwrap_err();
    assert!(
        matches!(
            err,
            CommandError::Iso(crate::iso9660::IsoError::OverlappingFiles { .. })
        ),
        "{err}"
    );
    assert!(err.to_string().contains("share disc sectors"), "{err}");
    assert!(
        !state.exists(),
        "not one byte of the shared extent was read, and nothing was created"
    );
}

/// The default caps are wired into the import: too many files, and more bytes than a CD holds.
#[test]
fn a_disc_listing_too_many_files_or_too_many_bytes_is_refused_end_to_end() {
    use crate::iso9660::{IsoError, MAX_FILES, MAX_FILE_BYTES};
    let tmp = tempfile::tempdir().unwrap();
    let images = tmp.path().join("images");
    let state = tmp.path().join("state");

    // MAX_FILES + 1 files (empty ones take no sectors, so the image stays small)
    let names: Vec<String> = (0..=MAX_FILES).map(|i| format!("F{i:05}.DAT")).collect();
    let mut files: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), &b""[..])).collect();
    let cnf = system_cnf("SCUS_941.63");
    files.push(("SYSTEM.CNF", cnf.as_slice()));
    let many = images.join("many.iso");
    fs::create_dir_all(&images).unwrap();
    fs::write(&many, build_iso_labeled(DISC_LABEL, &files)).unwrap();
    let err = import_disc(&disc_args(&many, &state, "ff7"), 1).unwrap_err();
    assert!(
        matches!(err, CommandError::Iso(IsoError::TooManyFiles { limit }) if limit == MAX_FILES),
        "{err}"
    );
    assert!(!state.exists(), "refused before anything was created");

    // one file that claims more bytes than a CD holds, in a sparse image big enough to hold it
    let mut iso = disc_with("SCUS_941.63", &[("BIG.DAT", b"tiny")]);
    let lba = crate::synthetic::lba_of(&iso, "BIG.DAT").unwrap();
    let root = u32::from_le_bytes(iso[16 * 2048 + 158..16 * 2048 + 162].try_into().unwrap())
        as usize
        * 2048;
    let mut off = 0;
    while iso[root + off] != 0 {
        let at = root + off;
        let name_len = iso[at + 32] as usize;
        if iso[at + 33..at + 33 + name_len].starts_with(b"BIG.DAT") {
            let size = (MAX_FILE_BYTES + 4096) as u32 as u64; // 1 GiB + 4 KiB, fits the u32 field
            iso[at + 10..at + 14].copy_from_slice(&(size as u32).to_le_bytes());
        }
        off += iso[at] as usize;
    }
    let big = images.join("big.iso");
    fs::write(&big, &iso).unwrap();
    let f = fs::OpenOptions::new().write(true).open(&big).unwrap();
    f.set_len(lba as u64 * 2048 + MAX_FILE_BYTES + 8192)
        .unwrap(); // sparse: no data is written
    drop(f);
    let err = import_disc(&disc_args(&big, &state, "ff7"), 2).unwrap_err();
    assert!(
        matches!(err, CommandError::Iso(IsoError::TooMuchData { limit }) if limit == MAX_FILE_BYTES),
        "{err}"
    );
    assert!(
        err.to_string().contains("more than a CD-ROM holds"),
        "{err}"
    );
    assert!(!state.exists());
}
