//! Compatibility of the on-disk library format.
//!
//! The two manifests below were written by the library as it was BEFORE the hardening of
//! findings 6-10 (captured from the unmodified writer, with synthetic content). The owner's real
//! library holds manifests of exactly this shape, so this test is the proof that:
//!
//! - a manifest of the old format still loads, and its blobs still verify;
//! - the writer still produces those very bytes for the same content (nothing about the format,
//!   the set-id derivation or the logical paths drifted);
//! - importing the same content again is a byte-for-byte no-op (the manifest is not even
//!   rewritten), so the owner's existing manifest is never touched by a re-import.
//!
//! Everything lives in temp directories: no real state directory is read or written.

use std::fs;
use std::io::Cursor;
use std::path::Path;

use fmv_core::{EditionProvenance, GameId, Platform, SourceRef};
use fmv_library::{set_id_for, Identification, Library, Manifest, SetHandle};

/// The set id of the owner's real Steam Edition set. The labels below derive exactly this id.
const OWNERS_SET_ID: &str = "ff7-pc-steam-edition-appid-3837-unknown-build-21140793-63e94f91";
const PS1_SET_ID: &str = "ff7-ps1-ps1-original-ntsc-u-unknown-d3e6983a";

const PC_MANIFEST: &str = r#"{
  "format_version": 1,
  "set_id": "ff7-pc-steam-edition-appid-3837-unknown-build-21140793-63e94f91",
  "provenance": {
    "game": "Ff7",
    "platform": "Pc",
    "edition": "Steam Edition (appid 3837340)",
    "region": "unknown",
    "language": "en",
    "revision": "build 21140793",
    "sources": [
      {
        "Install": {
          "path": "/synthetic/owned-src/ff7-steam"
        }
      }
    ],
    "modded": false
  },
  "entries": [
    {
      "logical_path": "data/battle/battle.lgp",
      "blake3": "16d7ed0829316d6eae9028eebff7901097c2f325ab5ed67bca4f46f6e60c9cdf",
      "size": 59,
      "source": {
        "Install": {
          "path": "/synthetic/owned-src/ff7-steam"
        }
      }
    },
    {
      "logical_path": "data/field/char.lgp",
      "blake3": "344567a304c742e5cdc676e933301d4de30378fe799d63d7617c5c5ed1eb2bb5",
      "size": 56,
      "source": {
        "Install": {
          "path": "/synthetic/owned-src/ff7-steam"
        }
      }
    },
    {
      "logical_path": "data/field/flevel.lgp",
      "blake3": "14371e53ab9593cf39dbbbfe78a646f90ff382fe18a7ae8a0cd4a080c9d1aec8",
      "size": 58,
      "source": {
        "Install": {
          "path": "/synthetic/owned-src/ff7-steam"
        }
      }
    },
    {
      "logical_path": "data/lang-en/battle/scene.bin",
      "blake3": "b888a7746a4630346f24680b97d71e64179810f956ae93eb1be54cd26353ba84",
      "size": 66,
      "source": {
        "Install": {
          "path": "/synthetic/owned-src/ff7-steam"
        }
      }
    },
    {
      "logical_path": "data/lang-en/kernel/kernel.bin",
      "blake3": "747de056cc33c4b1890ab38321edb0b7d201813f17c342a2a10e14f71d847d0a",
      "size": 67,
      "source": {
        "Install": {
          "path": "/synthetic/owned-src/ff7-steam"
        }
      }
    },
    {
      "logical_path": "data/lang-en/kernel/kernel2.bin",
      "blake3": "960724aff78258ffb482e84a6bec4c343369bba266630db8661242b46ee4d1c8",
      "size": 68,
      "source": {
        "Install": {
          "path": "/synthetic/owned-src/ff7-steam"
        }
      }
    }
  ],
  "imported_at": 1762600000,
  "importer_version": "fmv-import 0.1.0 (fmv-library 0.1.0)",
  "identification": {
    "verified": false,
    "method": "pc-steam-appmanifest",
    "caveats": [
      "edition was identified from the install files and Steam's appmanifest_3837340.acf (appid 3837340); it was not compared with hashes of known releases",
      "region is unknown: it cannot be told from the files"
    ]
  }
}"#;

const PS1_MANIFEST: &str = r#"{
  "format_version": 1,
  "set_id": "ff7-ps1-ps1-original-ntsc-u-unknown-d3e6983a",
  "provenance": {
    "game": "Ff7",
    "platform": "Ps1",
    "edition": "PS1 original",
    "region": "NTSC-U",
    "language": "en",
    "revision": "unknown",
    "sources": [
      {
        "Disc": {
          "index": 1,
          "serial": "SCUS-94163"
        }
      },
      {
        "Disc": {
          "index": 2,
          "serial": "SCUS-94164"
        }
      }
    ],
    "modded": false
  },
  "entries": [
    {
      "logical_path": "disc1/FIELD/MD1STIN.DAT",
      "blake3": "c71d4802814748611c316e6e8a2154c92d6d23cac3c03112b82dfbcf21b16963",
      "size": 60,
      "source": {
        "Disc": {
          "index": 1,
          "serial": "SCUS-94163"
        }
      }
    },
    {
      "logical_path": "disc1/SYSTEM.CNF",
      "blake3": "fc5a620fd79dc4246d6de4f1299c2c314479794ce1780e600019ae59f2418ebd",
      "size": 53,
      "source": {
        "Disc": {
          "index": 1,
          "serial": "SCUS-94163"
        }
      }
    },
    {
      "logical_path": "disc2/FIELD/MD1STIN.DAT",
      "blake3": "8cf9e15dc49989b848f8ca3d775a5a5220b1ea157b9d2bee4b26e537c0d0e5c7",
      "size": 60,
      "source": {
        "Disc": {
          "index": 2,
          "serial": "SCUS-94164"
        }
      }
    },
    {
      "logical_path": "disc2/SYSTEM.CNF",
      "blake3": "17354d7da6b1805c2bc738a58ef131ede8af8b2a400c2d266a3852daf9c1226c",
      "size": 53,
      "source": {
        "Disc": {
          "index": 2,
          "serial": "SCUS-94164"
        }
      }
    }
  ],
  "imported_at": 1762600100,
  "importer_version": "fmv-import 0.1.0 (fmv-library 0.1.0)",
  "identification": {
    "verified": false,
    "method": "ps1-serial-table",
    "caveats": [
      "edition, region and language come from the FF7 serial table, which is UNVERIFIED [verify]"
    ]
  }
}"#;

const IMPORTER: &str = "fmv-import 0.1.0 (fmv-library 0.1.0)";

fn blob_bytes(logical_path: &str) -> Vec<u8> {
    format!("GOLDEN-SYNTHETIC-BLOB {logical_path}: not game data").into_bytes()
}

fn pc_provenance() -> EditionProvenance {
    EditionProvenance {
        game: GameId::Ff7,
        platform: Platform::Pc,
        edition: "Steam Edition (appid 3837340)".into(),
        region: "unknown".into(),
        language: "en".into(),
        revision: "build 21140793".into(),
        sources: vec![],
        modded: false,
    }
}

fn ps1_provenance() -> EditionProvenance {
    EditionProvenance {
        game: GameId::Ff7,
        platform: Platform::Ps1,
        edition: "PS1 original".into(),
        region: "NTSC-U".into(),
        language: "en".into(),
        revision: "unknown".into(),
        sources: vec![],
        modded: false,
    }
}

fn pc_identification() -> Identification {
    Identification::unverified(
        "pc-steam-appmanifest",
        vec![
            "edition was identified from the install files and Steam's appmanifest_3837340.acf (appid 3837340); it was not compared with hashes of known releases".into(),
            "region is unknown: it cannot be told from the files".into(),
        ],
    )
}

fn ps1_identification() -> Identification {
    Identification::unverified(
        "ps1-serial-table",
        vec!["edition, region and language come from the FF7 serial table, which is UNVERIFIED [verify]".into()],
    )
}

const PC_FILES: [&str; 6] = [
    "data/battle/battle.lgp",
    "data/field/char.lgp",
    "data/field/flevel.lgp",
    "data/lang-en/battle/scene.bin",
    "data/lang-en/kernel/kernel.bin",
    "data/lang-en/kernel/kernel2.bin",
];

/// A library laid out the way an older build left it: the manifest text and the blobs, written
/// directly (not through the code under test).
fn library_with(tmp: &Path, set_id: &str, manifest: &str, logical_paths: &[String]) -> Library {
    let lib = Library::open(tmp.join("state")).unwrap();
    for p in logical_paths {
        let bytes = blob_bytes(p);
        let hex = blake3::hash(&bytes).to_hex().to_string();
        let blob = lib.blob_path(&hex).unwrap();
        fs::create_dir_all(blob.parent().unwrap()).unwrap();
        fs::write(blob, bytes).unwrap();
    }
    let dir = lib.library_dir().join("sets").join(set_id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("manifest.json"), manifest).unwrap();
    lib.with_importer_version(IMPORTER)
}

fn manifest_path(lib: &Library, set_id: &str) -> std::path::PathBuf {
    lib.library_dir()
        .join("sets")
        .join(set_id)
        .join("manifest.json")
}

#[test]
fn the_owners_set_id_is_still_derived_from_the_same_labels() {
    assert_eq!(set_id_for(&pc_provenance()), OWNERS_SET_ID);
    assert_eq!(set_id_for(&ps1_provenance()), PS1_SET_ID);
}

#[test]
fn a_manifest_of_the_old_format_loads_and_its_blobs_verify() {
    let tmp = tempfile::tempdir().unwrap();
    let paths: Vec<String> = PC_FILES.iter().map(|s| s.to_string()).collect();
    let lib = library_with(tmp.path(), OWNERS_SET_ID, PC_MANIFEST, &paths);

    assert_eq!(lib.list_sets().unwrap(), vec![OWNERS_SET_ID.to_string()]);
    let m = lib.load_manifest(OWNERS_SET_ID).unwrap();
    assert_eq!(m.format_version, 1);
    assert_eq!(m.entries.len(), 6);
    assert_eq!(m.imported_at, 1_762_600_000);
    assert_eq!(m.importer_version, IMPORTER);
    assert_eq!(m.provenance.edition, "Steam Edition (appid 3837340)");
    assert_eq!(m.provenance.revision, "build 21140793");
    assert_eq!(
        m.provenance.sources,
        vec![SourceRef::Install {
            path: "/synthetic/owned-src/ff7-steam".into()
        }]
    );
    assert_eq!(m.identification, Some(pc_identification()));
    assert!(!m.is_verified());
    assert!(m.discs.is_empty(), "an old manifest has no disc records");
    // the blobs those entries name are the ones that were written for them
    assert!(lib.verify_set(OWNERS_SET_ID).unwrap().is_empty());
    for p in PC_FILES {
        assert!(lib.get(OWNERS_SET_ID, p).unwrap().is_some(), "{p}");
    }
    // the field archive is found where the adapters look for it
    let flevel = lib
        .get(OWNERS_SET_ID, "data/field/flevel.lgp")
        .unwrap()
        .unwrap();
    assert_eq!(
        fs::read(flevel).unwrap(),
        blob_bytes("data/field/flevel.lgp")
    );

    let paths: Vec<String> = [
        "disc1/FIELD/MD1STIN.DAT",
        "disc1/SYSTEM.CNF",
        "disc2/FIELD/MD1STIN.DAT",
        "disc2/SYSTEM.CNF",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let tmp2 = tempfile::tempdir().unwrap();
    let lib2 = library_with(tmp2.path(), PS1_SET_ID, PS1_MANIFEST, &paths);
    let m = lib2.load_manifest(PS1_SET_ID).unwrap();
    assert_eq!(m.entries.len(), 4);
    assert_eq!(m.provenance.sources.len(), 2);
    assert_eq!(m.identification, Some(ps1_identification()));
    assert!(m.discs.is_empty());
    assert!(lib2.verify_set(PS1_SET_ID).unwrap().is_empty());
}

/// The writer's output for a manifest without the newer fields is the old output, byte for byte.
#[test]
fn the_writer_still_produces_the_old_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let paths: Vec<String> = PC_FILES.iter().map(|s| s.to_string()).collect();
    let lib = library_with(tmp.path(), OWNERS_SET_ID, PC_MANIFEST, &paths);
    let loaded: Manifest = lib.load_manifest(OWNERS_SET_ID).unwrap();
    assert_eq!(serde_json::to_string_pretty(&loaded).unwrap(), PC_MANIFEST);
    let tmp2 = tempfile::tempdir().unwrap();
    let lib2 = library_with(tmp2.path(), PS1_SET_ID, PS1_MANIFEST, &[]);
    let loaded = lib2.load_manifest(PS1_SET_ID).unwrap();
    assert_eq!(serde_json::to_string_pretty(&loaded).unwrap(), PS1_MANIFEST);

    // and a fresh import of the same content writes the very same file
    let fresh = tempfile::tempdir().unwrap();
    let lib3 = Library::open(fresh.path().join("state"))
        .unwrap()
        .with_importer_version(IMPORTER);
    let set = SetHandle::new(pc_provenance()).with_identification(pc_identification());
    assert_eq!(set.set_id, OWNERS_SET_ID);
    let mut batch = lib3.begin_import(&set, 1_762_600_000).unwrap();
    let install = SourceRef::Install {
        path: "/synthetic/owned-src/ff7-steam".into(),
    };
    for p in PC_FILES {
        let bytes = blob_bytes(p);
        batch
            .add_reader(
                p,
                &mut Cursor::new(bytes.clone()),
                Some(bytes.len() as u64),
                install.clone(),
            )
            .unwrap();
    }
    batch.commit().unwrap();
    assert_eq!(
        fs::read_to_string(manifest_path(&lib3, OWNERS_SET_ID)).unwrap(),
        PC_MANIFEST
    );
}

/// Re-importing what a set already holds never rewrites its manifest: the owner's existing
/// manifest stays exactly as it is.
#[test]
fn importing_the_same_content_again_leaves_the_manifest_byte_identical() {
    let tmp = tempfile::tempdir().unwrap();
    let paths: Vec<String> = PC_FILES.iter().map(|s| s.to_string()).collect();
    let lib = library_with(tmp.path(), OWNERS_SET_ID, PC_MANIFEST, &paths);
    let path = manifest_path(&lib, OWNERS_SET_ID);
    let before = fs::read(&path).unwrap();

    let set = SetHandle::new(pc_provenance()).with_identification(pc_identification());
    let install = SourceRef::Install {
        path: "/synthetic/owned-src/ff7-steam".into(),
    };
    let mut batch = lib.begin_import(&set, 1_999_999_999).unwrap();
    for p in PC_FILES {
        let bytes = blob_bytes(p);
        let o = batch
            .add_reader(
                p,
                &mut Cursor::new(bytes.clone()),
                Some(bytes.len() as u64),
                install.clone(),
            )
            .unwrap();
        assert!(
            !o.blob_created && !o.blob_repaired,
            "{p}: the stored blob is reused"
        );
    }
    batch.commit().unwrap();
    assert_eq!(
        fs::read(&path).unwrap(),
        before,
        "not one byte of the manifest may change"
    );

    // a genuinely new file is added without disturbing what was there
    let mut batch = lib.begin_import(&set, 1_999_999_999).unwrap();
    let extra = b"one more synthetic file".to_vec();
    batch
        .add_reader(
            "data/field/extra.lgp",
            &mut Cursor::new(extra.clone()),
            Some(extra.len() as u64),
            install,
        )
        .unwrap();
    batch.commit().unwrap();
    let after = lib.load_manifest(OWNERS_SET_ID).unwrap();
    assert_eq!(after.entries.len(), 7);
    let old: Manifest = serde_json::from_str(PC_MANIFEST).unwrap();
    for e in &old.entries {
        assert!(after.entries.contains(e), "{}", e.logical_path);
    }
    assert_eq!(after.imported_at, 1_999_999_999);
    assert!(lib.verify_set(OWNERS_SET_ID).unwrap().is_empty());
}
