//! End-to-end tests of the `fmv-import` binary on synthetic data only.
//!
//! Every child process gets a throw-away state directory three ways (`--state-dir`,
//! `FMV_STATE_DIR` and an `XDG_DATA_HOME` under the temp dir), so nothing can reach the real
//! user data directory.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use fmv_import::synthetic::{
    build_iso, build_iso_labeled, cue_for, steam_edition_library, system_cnf, wrap_raw, RawMode,
    DISC_LABEL, TEST_PAYLOAD_A, TEST_PAYLOAD_B,
};
use serde_json::Value;

struct Env {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    state: PathBuf,
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let state = root.join("state");
        Env {
            _tmp: tmp,
            root,
            state,
        }
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_fmv-import"));
        cmd.env("FMV_STATE_DIR", &self.state)
            .env("XDG_DATA_HOME", self.root.join("xdg"))
            .env("HOME", self.root.join("home"));
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }

    fn run_with_state(&self, args: &[&str]) -> Output {
        self.command()
            .args(args)
            .arg("--state-dir")
            .arg(&self.state)
            .output()
            .unwrap()
    }
}

fn json(out: &Output) -> Value {
    assert!(
        out.status.success(),
        "command failed: {}\nstdout: {}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

fn touch(root: &Path, rel: &str, bytes: &[u8]) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, bytes).unwrap();
}

/// A made-up disc whose volume label does NOT mark it as synthetic, so that the importer treats
/// it like a real disc (serial table, sessions). The bytes are still test data.
fn disc(env: &Env, name: &str, exe: &str, tag: &str) -> PathBuf {
    let extra = format!("{tag}: synthetic");
    let iso = build_iso_labeled(
        DISC_LABEL,
        &[
            ("SYSTEM.CNF", &system_cnf(exe)),
            ("FIELD/MD1STIN.DAT", TEST_PAYLOAD_A),
            ("BATTLE/SCENE.BIN", TEST_PAYLOAD_B),
            ("DISC.TXT", extra.as_bytes()),
        ],
    );
    write_image(env, name, &iso)
}

/// An image labelled SYNTHETIC_TEST, as `build_iso` makes them: the importer must call it FIXTURE.
fn synthetic_disc(env: &Env, name: &str) -> PathBuf {
    let iso = build_iso(&[
        ("SYSTEM.CNF", &system_cnf("SCUS_941.63")),
        ("FIELD/MD1STIN.DAT", TEST_PAYLOAD_A),
    ]);
    write_image(env, name, &iso)
}

fn write_image(env: &Env, name: &str, iso: &[u8]) -> PathBuf {
    let dir = env.root.join("images");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(format!("{name}.bin")),
        wrap_raw(iso, RawMode::Mode2Form1),
    )
    .unwrap();
    fs::write(
        dir.join(format!("{name}.cue")),
        cue_for(&format!("{name}.bin"), "MODE2/2352"),
    )
    .unwrap();
    dir.join(format!("{name}.cue"))
}

fn pc_scan_import_and_list_flow() {
    let env = Env::new();
    let install = env.root.join("FF7");
    for f in [
        "ff7_en.exe",
        "data/field/flevel.lgp",
        "data/field/char.lgp",
        "data/battle/scene.bin",
    ] {
        touch(&install, f, b"");
    }
    touch(&install, "FFNx.dll", b"");

    // scan-pc: JSON on stdout, and it never touches the state directory
    let report = json(&env.run(&["scan-pc", install.to_str().unwrap()]));
    assert_eq!(report["read_only"], true);
    assert_eq!(report["modded"], true);
    assert_eq!(report["m1"]["ready"], true);
    assert_eq!(report["m2"]["ready"], false);
    assert_eq!(report["edition_guess"]["verified"], false);
    assert!(
        !env.state.exists(),
        "scan-pc must not create the state directory"
    );
    assert!(!env.root.join("xdg").exists());

    // import-pc
    let imported =
        json(&env.run_with_state(&["import-pc", install.to_str().unwrap(), "--region", "NTSC-U"]));
    assert_eq!(imported["provenance"]["modded"], true);
    assert_eq!(imported["provenance"]["region"], "NTSC-U");
    assert_eq!(imported["source_files_unchanged"], true);
    assert_eq!(imported["totals"]["files"], 4);
    assert!(env.state.join("library").join("sets").is_dir());

    // list (state dir from FMV_STATE_DIR this time)
    let listed = json(&env.run(&["list"]));
    assert_eq!(listed["library_exists"], true);
    assert_eq!(listed["sets"].as_array().unwrap().len(), 1);
    assert_eq!(listed["sets"][0]["label"], "FINAL FANTASY VII");
    assert_eq!(listed["sets"][0]["modded"], true);
    // the guess is recorded as a guess, and the listing does not print the install path ...
    assert_eq!(listed["sets"][0]["identification_verified"], false);
    assert_eq!(listed["sets"][0]["identification"]["method"], "pc-file-presence");
    let install_text = install.to_str().unwrap();
    assert!(
        !listed.to_string().contains(install_text),
        "list must not print the install path by default: {listed}"
    );
    // ... unless asked to
    let with_paths = json(&env.run(&["list", "--show-paths"]));
    assert!(with_paths.to_string().contains(install_text));

    // a PC install without flevel.lgp is refused with a non-zero exit
    let bare = env.root.join("bare");
    touch(&bare, "ff7.exe", b"");
    let out = env.run_with_state(&["import-pc", bare.to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("flevel.lgp"));
}

fn disc_session_flow_and_family_refusal() {
    let env = Env::new();
    let d1 = disc(&env, "d1", "SCUS_941.63", "one");
    let pal2 = disc(&env, "pal2", "SCES_108.67", "pal");
    let d2 = disc(&env, "d2", "SCUS_941.64", "two");

    let first =
        json(&env.run_with_state(&["import-disc", d1.to_str().unwrap(), "--set-name", "ff7"]));
    assert_eq!(first["disc"]["index"], 1);
    assert_eq!(first["disc"]["serial"], "SCUS-94163");
    assert_eq!(first["disc"]["serial_table_verified"], false);
    assert_eq!(first["totals"]["files"], 4);

    let refused = env.run_with_state(&["import-disc", pal2.to_str().unwrap(), "--set-name", "ff7"]);
    assert!(!refused.status.success());
    let err = String::from_utf8_lossy(&refused.stderr);
    assert!(
        err.contains("disc belongs to") && err.contains("session is"),
        "{err}"
    );

    json(&env.run_with_state(&["import-disc", d2.to_str().unwrap(), "--set-name", "ff7"]));
    let status = json(&env.run_with_state(&["import-status", "ff7"]));
    assert_eq!(status["expected_discs"], 3);
    assert_eq!(status["imported"].as_array().unwrap().len(), 2);
    assert_eq!(status["missing"].as_array().unwrap().len(), 1);
    assert_eq!(status["missing"][0]["index"], 3);
    assert_eq!(status["complete"], false);

    let missing = env.run_with_state(&["import-status", "nope"]);
    assert!(!missing.status.success());

    // an image labelled SYNTHETIC_TEST is FIXTURE content: never FF7, and `list` says so
    let fake = synthetic_disc(&env, "fake");
    let fx = json(&env.run_with_state(&["import-disc", fake.to_str().unwrap(), "--set-name", "fx"]));
    assert_eq!(fx["disc"]["fixture"], true);
    assert_eq!(fx["disc"]["kind"], "fixture");
    let listed = json(&env.run_with_state(&["list"]));
    let fixture_sets: Vec<&Value> = listed["sets"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["fixture"] == true)
        .collect();
    assert_eq!(fixture_sets.len(), 1, "{listed}");
    assert!(fixture_sets[0]["label"].as_str().unwrap().contains("FIXTURE"));
}

/// An install with several languages: refused until one is chosen, then one set per language.
fn multi_language_install_needs_a_language() {
    let env = Env::new();
    let install = env.root.join("steam");
    for f in [
        "ff7_en.exe",
        "ff7_fr.exe",
        "data/lang-en/field/flevel.lgp",
        "data/lang-fr/field/flevel.lgp",
    ] {
        touch(&install, f, b"synthetic placeholder");
    }
    let report = json(&env.run(&["scan-pc", install.to_str().unwrap()]));
    assert_eq!(report["language_selection_required"], true);
    assert_eq!(report["languages"].as_array().unwrap().len(), 2);

    let refused = env.run_with_state(&["import-pc", install.to_str().unwrap()]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("--language"));
    assert!(!env.state.exists(), "a refusal creates nothing");

    let fr = json(&env.run_with_state(&[
        "import-pc",
        install.to_str().unwrap(),
        "--language",
        "fr",
    ]));
    assert_eq!(fr["language"], "fr");
    assert_eq!(fr["provenance"]["language"], "fr");
    assert_eq!(fr["languages_left_out"][0], "en");
    let names: Vec<&str> = fr["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["logical_path"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["data/lang-fr/field/flevel.lgp", "ff7_fr.exe"]);
}

/// The Steam Edition's install root, as `scan-pc` and `import-pc` take it from the command line:
/// no labels, no flags, and the same set the explicit labels made.
fn steam_edition_install_root_flow() {
    let env = Env::new();
    // a fake Steam library (placeholder files + a fake appmanifest), away from the state directory
    let install = steam_edition_library(&env.root.join("steamlib"), true);
    let workingdir = install.join("ff7/workingdir");
    let install_text = install.to_str().unwrap();
    let owners_set = "ff7-pc-steam-edition-appid-3837-unknown-build-21140793-63e94f91";

    // scan-pc: read-only, names the edition with its evidence, defaults the language out loud
    let report = json(&env.run(&["scan-pc", install_text]));
    assert_eq!(report["read_only"], true);
    assert_eq!(report["data_dir"], "ff7/workingdir/data");
    assert_eq!(
        report["edition_guess"]["edition"],
        "Steam Edition (appid 3837340)"
    );
    assert_eq!(report["edition_guess"]["confidence"], "high");
    assert_eq!(report["edition_guess"]["revision"], "build 21140793");
    assert_eq!(report["edition_guess"]["verified"], false);
    assert_eq!(report["edition_guess"]["steam"]["buildid"], "21140793");
    assert_eq!(report["language_guess"], "en");
    assert_eq!(report["language_defaulted"], true);
    assert_eq!(report["language_selection_required"], false);
    assert_eq!(report["modded"], false);
    assert!(
        report["m1"]["ready"] == true && report["m2"]["ready"] == true,
        "{report}"
    );
    assert!(report["warnings"][0]
        .as_str()
        .unwrap()
        .starts_with("language defaulted to"));
    assert!(
        !env.state.exists(),
        "scan-pc must not create the state directory"
    );
    assert!(!env.root.join("xdg").exists());
    // the folder that holds data/ scans alike
    let inner = json(&env.run(&["scan-pc", workingdir.to_str().unwrap()]));
    assert_eq!(inner["data_dir"], "data");
    assert_eq!(
        inner["edition_guess"]["edition"],
        report["edition_guess"]["edition"]
    );
    assert_eq!(
        inner["edition_guess"]["revision"],
        report["edition_guess"]["revision"]
    );

    // import-pc with nothing but the install root
    let imported = json(&env.run_with_state(&["import-pc", install_text]));
    assert_eq!(imported["set_id"], owners_set);
    assert_eq!(
        imported["provenance"]["edition"],
        "Steam Edition (appid 3837340)"
    );
    assert_eq!(imported["provenance"]["region"], "unknown");
    assert_eq!(imported["provenance"]["language"], "en");
    assert_eq!(imported["provenance"]["revision"], "build 21140793");
    assert_eq!(imported["totals"]["files"], 6);
    assert_eq!(imported["source_files_unchanged"], true);
    assert_eq!(imported["identification"]["verified"], false);
    assert!(
        !imported.to_string().contains(install_text),
        "the result names no install path: {imported}"
    );

    // the owner's explicit labels, from the folder that holds data/: the same set, nothing added
    let explicit = json(&env.run_with_state(&[
        "import-pc",
        workingdir.to_str().unwrap(),
        "--edition",
        "Steam Edition (appid 3837340)",
        "--language",
        "en",
        "--revision",
        "build 21140793",
    ]));
    assert_eq!(explicit["set_id"], owners_set);
    assert_eq!(explicit["totals"]["entries_added"], 0);
    assert_eq!(explicit["totals"]["entries_unchanged"], 6);

    // another language is a set of its own, with that language's field archive
    let fr = json(&env.run_with_state(&["import-pc", install_text, "--language", "fr"]));
    assert_ne!(fr["set_id"], owners_set);
    assert_eq!(fr["provenance"]["edition"], "Steam Edition (appid 3837340)");
    assert_eq!(fr["provenance"]["revision"], "build 21140793");
    let names: Vec<&str> = fr["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["logical_path"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"data/lang-fr/field/flevel.lgp"),
        "{names:?}"
    );
    assert!(!names.contains(&"data/field/flevel.lgp"), "{names:?}");

    // list shows both, recorded as unverified, without the install path
    let listed = json(&env.run(&["list"]));
    assert_eq!(listed["sets"].as_array().unwrap().len(), 2);
    for set in listed["sets"].as_array().unwrap() {
        assert_eq!(set["edition"], "Steam Edition (appid 3837340)");
        assert_eq!(set["identification_verified"], false);
    }
    assert!(!listed.to_string().contains(install_text));

    // nothing was written into the Steam library
    assert!(!install.join("state").exists() && !workingdir.join("library").exists());
}

fn help_says_physical_drives_are_out_of_scope() {
    let env = Env::new();
    for args in [&["--help"][..], &["import-disc", "--help"][..]] {
        let out = env.run(args);
        assert!(out.status.success());
        // help text is wrapped by clap: compare with whitespace collapsed
        let text = String::from_utf8_lossy(&out.stdout)
            .to_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(text.contains("physical drive"), "{args:?}: {text}");
        assert!(text.contains("out of scope"), "{args:?}: {text}");
    }
    assert!(!env.state.exists(), "--help must not create anything");
}

fn default_state_dir_follows_xdg_when_nothing_else_is_set() {
    let env = Env::new();
    let out = Command::new(env!("CARGO_BIN_EXE_fmv-import"))
        .arg("list")
        .env_remove("FMV_STATE_DIR")
        .env("XDG_DATA_HOME", env.root.join("xdg"))
        .env("HOME", env.root.join("home"))
        .output()
        .unwrap();
    let listed = json(&out);
    assert_eq!(listed["library_exists"], false);
    assert_eq!(
        Path::new(listed["state_dir"].as_str().unwrap()),
        env.root.join("xdg").join("ff-multiverse")
    );
    assert!(
        !env.root.join("xdg").exists(),
        "list must not create the default state directory"
    );
}

/// A crafted image that claims a genuine disc is refused in plain words, `--replace-disc` is the
/// documented way to replace it, and `remove-set` drops a set only when told to.
fn replace_disc_and_remove_set_flow() {
    let env = Env::new();
    let genuine = disc(&env, "genuine", "SCUS_941.63", "the real disc");
    let crafted = disc(&env, "crafted", "SCUS_941.63", "someone else's image");
    let first = json(&env.run_with_state(&["import-disc", genuine.to_str().unwrap(), "--set-name", "ff7"]));
    let set_id = first["set_id"].as_str().unwrap().to_string();
    assert_eq!(first["content"]["files"], 4);
    assert_eq!(first["content"]["blake3"].as_str().unwrap().len(), 64);
    assert!(first["replaced"].is_null());

    // another image of "the same disc": refused with a clear message, nothing changed
    let out = env.run_with_state(&["import-disc", crafted.to_str().unwrap(), "--set-name", "other"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("different disc content for SCUS-94163 disc 1") && err.contains("--replace-disc"),
        "{err}"
    );
    assert!(!err.contains("EntryConflict") && !err.contains("not supported yet"), "{err}");
    let listed = json(&env.run(&["list"]));
    assert_eq!(listed["sets"][0]["discs"][0]["blake3"], first["content"]["blake3"], "{listed}");

    // the explicit way
    let replaced = json(&env.run_with_state(&[
        "import-disc",
        crafted.to_str().unwrap(),
        "--set-name",
        "other",
        "--replace-disc",
    ]));
    assert_eq!(replaced["replaced"]["entries"], 4);
    assert_eq!(replaced["replaced"]["serial"], "SCUS-94163");
    assert_ne!(replaced["content"]["blake3"], first["content"]["blake3"]);
    let help = String::from_utf8_lossy(&env.run(&["import-disc", "--help"]).stdout).to_string();
    assert!(help.contains("--replace-disc"), "{help}");

    // remove-set: no --yes, no change
    let out = env.run_with_state(&["remove-set", &set_id]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--yes") && err.contains(&set_id) && err.contains("Nothing was changed"), "{err}");
    assert_eq!(json(&env.run(&["list"]))["sets"].as_array().unwrap().len(), 1);
    // an id that is no set id is refused outright
    let out = env.run_with_state(&["remove-set", "../library", "--yes"]);
    assert!(!out.status.success() && String::from_utf8_lossy(&out.stderr).contains("invalid set id"));
    assert!(env.state.join("library/blobs").is_dir());
    // with --yes: the set and the sessions that point at it go, the blobs and the images stay
    let blobs_before = count_files(&env.state.join("library/blobs"));
    let removed = json(&env.run_with_state(&["remove-set", &set_id, "--yes"]));
    assert_eq!(removed["command"], "remove-set");
    assert_eq!(removed["entries"], 4);
    let mut gone: Vec<&str> = removed["sessions_removed"].as_array().unwrap().iter().map(|s| s.as_str().unwrap()).collect();
    gone.sort_unstable();
    assert_eq!(gone, ["ff7", "other"]);
    assert!(removed["blobs"].as_str().unwrap().starts_with("kept"));
    assert_eq!(json(&env.run(&["list"]))["sets"].as_array().unwrap().len(), 0);
    assert_eq!(count_files(&env.state.join("library/blobs")), blobs_before);
    assert!(genuine.is_file() && crafted.is_file(), "the images are never touched");
    let missing = env.run_with_state(&["remove-set", &set_id, "--yes"]);
    assert!(!missing.status.success() && String::from_utf8_lossy(&missing.stderr).contains("not found"));
}

fn count_files(dir: &Path) -> usize {
    fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| if e.path().is_dir() { count_files(&e.path()) } else { 1 })
                .sum()
        })
        .unwrap_or(0)
}

/// An empty state directory is an error, never "use the default library".
fn an_empty_state_dir_is_refused_not_replaced_by_the_default() {
    let env = Env::new();
    let install = env.root.join("FF7");
    touch(&install, "ff7.exe", b"x");
    touch(&install, "data/field/flevel.lgp", b"x");

    // FMV_STATE_DIR set but empty: refused by every command that would have used it
    for args in [&["list"][..], &["import-status", "ff7"][..], &["remove-set", "x-00000000", "--yes"][..]] {
        let out = Command::new(env!("CARGO_BIN_EXE_fmv-import"))
            .args(args)
            .env("FMV_STATE_DIR", "")
            .env("XDG_DATA_HOME", env.root.join("xdg"))
            .env("HOME", env.root.join("home"))
            .output()
            .unwrap();
        assert!(!out.status.success(), "{args:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("FMV_STATE_DIR") && err.contains("empty"), "{args:?}: {err}");
    }
    let out = Command::new(env!("CARGO_BIN_EXE_fmv-import"))
        .args(["import-pc", install.to_str().unwrap()])
        .env("FMV_STATE_DIR", "")
        .env("XDG_DATA_HOME", env.root.join("xdg"))
        .env("HOME", env.root.join("home"))
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(!env.root.join("xdg").exists(), "the default library was not touched or created");
    assert!(!env.root.join("home").exists());

    // an empty --state-dir (the shell variable that was never set) is refused for every command
    for args in [&["list"][..], &["import-status", "ff7"][..], &["scan-pc", install.to_str().unwrap()][..]] {
        let out = env.command().args(args).args(["--state-dir", ""]).output().unwrap();
        assert!(!out.status.success(), "{args:?}");
    }
    let out = env.command().args(["import-pc", install.to_str().unwrap(), "--state-dir", ""]).output().unwrap();
    assert!(!out.status.success());
    assert!(!env.state.exists() && !env.root.join("xdg").exists());

    // scan-pc never looks at the environment's state directory, so an empty one is no business of its
    let out = Command::new(env!("CARGO_BIN_EXE_fmv-import"))
        .args(["scan-pc", install.to_str().unwrap()])
        .env("FMV_STATE_DIR", "")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

/// A link out of the install is reported by `scan-pc` and never imported.
#[cfg(unix)]
fn links_out_of_an_install_are_reported_and_never_imported() {
    let env = Env::new();
    let install = env.root.join("FF7");
    touch(&install, "ff7.exe", b"exe");
    touch(&install, "data/field/char.lgp", b"char");
    touch(&env.root.join("elsewhere"), "secret.txt", b"NOT-A-REAL-SECRET");
    std::os::unix::fs::symlink(env.root.join("elsewhere/secret.txt"), install.join("data/field/flevel.lgp")).unwrap();
    let report = json(&env.run(&["scan-pc", install.to_str().unwrap()]));
    assert_eq!(report["refused_links"][0]["relative_path"], "data/field/flevel.lgp");
    assert!(report["refused_links"][0]["reason"].as_str().unwrap().contains("outside"));
    assert_eq!(report["m1"]["ready"], false);
    assert!(!report.to_string().contains("secret.txt"), "the target is not reported: {report}");
    let out = env.run_with_state(&["import-pc", install.to_str().unwrap()]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("symbolic link") && err.contains("flevel.lgp"), "{err}");
    assert!(!env.state.exists(), "a refusal creates nothing");
}

/// scan / import-pc / list, then a three-disc session with a foreign-region refusal.
#[test]
fn import_flows_end_to_end() {
    replace_disc_and_remove_set_flow();
    an_empty_state_dir_is_refused_not_replaced_by_the_default();
    #[cfg(unix)]
    links_out_of_an_install_are_reported_and_never_imported();
    pc_scan_import_and_list_flow();
    disc_session_flow_and_family_refusal();
    multi_language_install_needs_a_language();
    steam_edition_install_root_flow();
}

/// `--help` wording and the default state directory, without touching the real user dirs.
#[test]
fn help_text_and_default_state_dir() {
    help_says_physical_drives_are_out_of_scope();
    default_state_dir_follows_xdg_when_nothing_else_is_set();
}

/// Audio through the binary: `scan-pc` summarises it in the last key of its report, `import-pc`
/// takes it, and importing again with the same labels adds it to a library that holds the install
/// without it. `--help` says how.
#[test]
fn audio_is_scanned_imported_and_added_to_an_existing_library_by_importing_again() {
    let env = Env::new();
    let install = env.root.join("FF7");
    for f in ["ff7_en.exe", "data/field/flevel.lgp", "data/field/char.lgp"] {
        touch(&install, f, b"synthetic placeholder");
    }
    let dir = install.to_str().unwrap();

    // without audio: the key is there, all zero, and nothing else says a word about audio
    let out = env.run(&["scan-pc", dir]);
    let report = json(&out);
    assert_eq!(
        report["audio"],
        serde_json::json!({
            "ogg_files": 0, "wav_files": 0, "music_idx": false, "audio_dat": false,
            "audio_fmt": false, "total_bytes": 0, "skipped_files": 0
        })
    );
    assert_eq!(report["warnings"], serde_json::json!([]));
    let first = json(&env.run_with_state(&["import-pc", dir]));
    let set_id = first["set_id"].as_str().unwrap().to_string();
    assert_eq!(first["totals"]["files"], 3);

    // the audio arrives
    touch(&install, "data/music_ogg/bat.ogg", b"synthetic ogg");
    touch(&install, "data/music_ogg/victory.ogg", b"synthetic ogg two");
    touch(&install, "data/music/heart.wav", b"synthetic wav");
    touch(&install, "data/music/music.idx", b"idx");
    touch(&install, "data/sound/audio.dat", b"sound bank");
    touch(&install, "data/sound/audio.fmt", b"table");
    let out = env.run(&["scan-pc", dir]);
    let report = json(&out);
    assert_eq!(
        report["audio"],
        serde_json::json!({
            "ogg_files": 2, "wav_files": 1, "music_idx": true, "audio_dat": true,
            "audio_fmt": true, "total_bytes": 13 + 17 + 13 + 3 + 10 + 5, "skipped_files": 0
        })
    );
    // `audio` is the last key of the printed report (new keys only ever come at the end)
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let tail = &text[text.rfind("\"audio\"").unwrap()..];
    assert_eq!(
        (tail.matches('{').count(), tail.matches('}').count()),
        (1, 2),
        "{tail}"
    );
    assert!(text.rfind("\"notes\"").unwrap() < text.rfind("\"audio\"").unwrap());
    let audio_entries = report["would_import"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| w["role"] == "audio")
        .count();
    assert_eq!(audio_entries, 6);

    // importing again with the same labels adds the audio to the set that is there
    let second = json(&env.run_with_state(&["import-pc", dir]));
    assert_eq!(second["set_id"], set_id.as_str());
    assert_eq!(second["totals"]["entries_added"], 6);
    assert_eq!(second["totals"]["entries_unchanged"], 3);
    assert_eq!(second["identification"], first["identification"]);
    let listed = json(&env.run_with_state(&["list"]));
    let sets = listed["sets"].as_array().unwrap();
    assert_eq!(sets.len(), 1, "{listed}");
    assert_eq!(sets[0]["files"], 9);
    assert_eq!(sets[0]["identification"], first["identification"]);

    // the help says how to add audio to an existing library
    for args in [&["--help"][..], &["import-pc", "--help"][..]] {
        let text = String::from_utf8_lossy(&env.run(args).stdout)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            text.contains("import-pc again on the same install with the same --edition/--region/--language/--revision labels"),
            "{args:?}: {text}"
        );
        assert!(
            text.contains("second set") && text.contains("ambiguous"),
            "{args:?}: {text}"
        );
    }
    let scan_help = String::from_utf8_lossy(&env.run(&["scan-pc", "--help"]).stdout).to_string();
    assert!(scan_help.contains("audio"), "{scan_help}");
}
