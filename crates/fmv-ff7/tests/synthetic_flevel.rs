//! End-to-end checks of the FF7 adapter through its public API only, over a SYNTHETIC
//! `flevel.lgp` built in this file. No game data is involved; nothing here says anything
//! about real FF7 files (the decoders are UNVERIFIED AGAINST OWNED MEDIA). The archive is
//! labelled the way the import pipeline labels synthetic content (`Platform::Synthetic` and a
//! `SourceRef::Fixture` source), so the adapter reports every area as FIXTURE.
//!
//! The archive is assembled with an independent, literal-only LZSS packer (no references),
//! so it also exercises the decoder against a stream the crate's own test compressor did
//! not produce.

use fmv_adapter_api::{AdapterError, GameAdapter, MoveResult};
use fmv_core::{EditionProvenance, GameId, Platform, Pos, SourceRef};
use fmv_ff7::{adapter_version, area_fingerprint, Ff7Adapter, FINGERPRINT_RULE_VERSION, VERIFICATION_STATUS};
use std::io::Cursor;
use std::sync::Arc;

fn provenance() -> EditionProvenance {
    EditionProvenance {
        game: GameId::Ff7,
        platform: Platform::Synthetic,
        edition: "synthetic".into(),
        region: "none".into(),
        language: "none".into(),
        revision: "test".into(),
        sources: vec![SourceRef::Fixture],
        modded: false,
    }
}

/// LZSS stream made only of literals: control byte (bit set = literal) + up to 8 bytes.
fn pack_literals(data: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    for chunk in data.chunks(8) {
        body.push(0xFFu8 >> (8 - chunk.len()));
        body.extend_from_slice(chunk);
    }
    let mut out = (body.len() as u32).to_le_bytes().to_vec();
    out.extend_from_slice(&body);
    out
}

/// A minimal 9-section field whose only meaningful section is the walkmesh: a `w` x `h`
/// rectangle at height `z`, as two triangles.
fn field_with_rectangle(w: i16, h: i16, z: i16) -> Vec<u8> {
    let tri = |pts: [(i16, i16); 3]| -> Vec<u8> {
        let mut v = Vec::new();
        for (x, y) in pts {
            for c in [x, y, z, z] {
                v.extend_from_slice(&c.to_le_bytes());
            }
        }
        v
    };
    let mut walk = 2u32.to_le_bytes().to_vec();
    walk.extend(tri([(0, 0), (w, 0), (w, h)]));
    walk.extend(tri([(0, 0), (w, h), (0, h)]));
    for access in [[0xFFFFu16, 0xFFFF, 1], [0, 0xFFFF, 0xFFFF]] {
        for a in access {
            walk.extend_from_slice(&a.to_le_bytes());
        }
    }
    // Other sections: tiny arbitrary payloads (the adapter only needs the walkmesh to host).
    let sections: [Vec<u8>; 9] = [
        vec![1, 2, 3, 4],
        vec![0; 38],
        vec![0; 6],
        vec![0; 16],
        walk,
        vec![0; 4],
        vec![0; 48],
        vec![0; 740],
        vec![0; 8],
    ];
    let mut out = vec![0, 0];
    out.extend_from_slice(&9u32.to_le_bytes());
    let mut offset = 2 + 4 + 4 * 9;
    for s in &sections {
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += 4 + s.len();
    }
    for s in &sections {
        out.extend_from_slice(&(s.len() as u32).to_le_bytes());
        out.extend_from_slice(s);
    }
    out
}

fn lgp(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let toc_end = 16 + entries.len() * 27;
    let mut offset = toc_end + 3600 + 2;
    let mut out = b"\0\0SQUARESOFT".to_vec();
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for (name, data) in entries {
        let mut n = [0u8; 20];
        n[..name.len()].copy_from_slice(name.as_bytes());
        out.extend_from_slice(&n);
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        out.push(0x0E);
        out.extend_from_slice(&0u16.to_le_bytes());
        offset += 24 + data.len();
    }
    out.extend(vec![0u8; 3600]);
    out.extend_from_slice(&0u16.to_le_bytes());
    for (name, data) in entries {
        let mut n = [0u8; 20];
        n[..name.len()].copy_from_slice(name.as_bytes());
        out.extend_from_slice(&n);
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(data);
    }
    out.extend_from_slice(b"FINAL FANTASY7");
    out
}

fn synthetic_flevel() -> Vec<u8> {
    lgp(&[
        ("maplist", vec![2, 0, b'a', b'b']),
        ("stationx", pack_literals(&field_with_rectangle(400, 300, 0))),
        ("cliffy", pack_literals(&field_with_rectangle(200, 800, 50))),
        ("notafield", pack_literals(b"this is not a field file at all, just text")),
    ])
}

/// The stored (compressed) bytes of a field entry, as `synthetic_flevel` stores them.
fn stored(field: &[u8]) -> Vec<u8> {
    pack_literals(field)
}

#[test]
fn lists_loads_and_validates_two_fields_through_the_trait() {
    let adapter: Box<dyn GameAdapter> =
        Box::new(Ff7Adapter::from_flevel_reader(Cursor::new(synthetic_flevel()), provenance()).unwrap());
    assert_eq!(adapter.game(), GameId::Ff7);
    assert_eq!(VERIFICATION_STATUS, "VERIFIED ON OWNED FF7 STEAM EDITION (EN) 2026-11-08; OTHER EDITIONS UNVERIFIED");

    // Listing does not decode: every plausibly named entry is offered (sorted), only the name
    // list is left out. An entry that is not a field is listed and refuses to load.
    let areas = adapter.list_areas();
    let keys: Vec<&str> = areas.iter().map(|a| a.id.key.as_str()).collect();
    assert_eq!(keys, ["cliffy", "notafield", "stationx"]);
    assert!(areas.iter().all(|a| a.id.game == GameId::Ff7 && a.provenance == provenance()));
    // synthetic content is labelled FIXTURE everywhere it is surfaced
    assert!(areas.iter().all(|a| a.fixture && a.display_name.starts_with("FIXTURE:")), "{areas:?}");

    // list_areas and load_area agree on the fingerprint, and it is the documented rule over the
    // stored entry and the provenance.
    let station = adapter.load_area("stationx").unwrap();
    let listed = areas.iter().find(|a| a.id.key == "stationx").unwrap();
    assert_eq!(station.info().fingerprint, listed.fingerprint);
    assert_eq!(listed.fingerprint, area_fingerprint(&provenance().summary(), &stored(&field_with_rectangle(400, 300, 0))));
    assert_ne!(listed.fingerprint, areas[0].fingerprint);
    assert!(station.info().fixture && station.info().display_name.starts_with("FIXTURE:"));

    // Two "players": both spawn on the mesh, off-mesh moves are rejected.
    for spawn in station.spawn_points().into_iter().take(2) {
        assert!(matches!(station.validate_move(spawn, spawn), MoveResult::Accepted(_)));
    }
    let from = Pos::new(100.0, 100.0, 0.0);
    assert!(matches!(station.validate_move(from, Pos::new(399.0, 10.0, 0.0)), MoveResult::Accepted(_)));
    assert_eq!(station.validate_move(from, Pos::new(401.0, 10.0, 0.0)), MoveResult::Corrected(from));
    assert_eq!(station.bounds().max, Pos::new(400.0, 300.0, 0.0));

    // The second field has its own height.
    let cliff = adapter.load_area("CLIFFY").unwrap();
    match cliff.validate_move(Pos::new(10.0, 10.0, 50.0), Pos::new(150.0, 700.0, 0.0)) {
        MoveResult::Accepted(p) => assert!((p.z - 50.0).abs() < 1e-3),
        other => panic!("expected Accepted, got {other:?}"),
    }

    // Entries that are not fields say why when loaded.
    assert!(matches!(adapter.load_area("maplist"), Err(AdapterError::Decode(_))));
    assert!(matches!(adapter.load_area("notafield"), Err(AdapterError::Decode(_))));
    assert!(matches!(adapter.load_area("nope"), Err(AdapterError::AreaNotFound(_))));
}

#[test]
fn the_same_archive_under_a_pc_provenance_is_real_and_has_a_different_fingerprint() {
    let pc = EditionProvenance { platform: Platform::Pc, sources: vec![], ..provenance() };
    let real = Ff7Adapter::from_flevel_reader(Cursor::new(synthetic_flevel()), pc).unwrap();
    let fake = Ff7Adapter::from_flevel_reader(Cursor::new(synthetic_flevel()), provenance()).unwrap();
    let (real, fake) = (real.list_areas(), fake.list_areas());
    // the adapter trusts the provenance it is given (it cannot see "synthetic" in the bytes)
    assert!(real.iter().all(|a| !a.fixture && !a.display_name.contains("FIXTURE")));
    // the provenance is inside the fingerprint, so the two cannot be mistaken for each other
    assert_eq!(real.len(), fake.len());
    assert!(real.iter().zip(&fake).all(|(r, f)| r.id == f.id && r.fingerprint != f.fingerprint));
    assert!(adapter_version().ends_with(&format!("fingerprint-rule/{FINGERPRINT_RULE_VERSION}")));
}

#[test]
fn opens_a_file_path_read_only_and_serves_threads() {
    let bytes = synthetic_flevel();
    let path = std::env::temp_dir().join(format!("fmv-ff7-it-{}.lgp", std::process::id()));
    std::fs::write(&path, &bytes).unwrap();

    let adapter: Arc<dyn GameAdapter> = Arc::new(Ff7Adapter::from_flevel_path(&path, provenance()).unwrap());
    let handles: Vec<_> = (0..4)
        .map(|i| {
            let adapter = Arc::clone(&adapter);
            std::thread::spawn(move || {
                let key = if i % 2 == 0 { "stationx" } else { "cliffy" };
                let area = adapter.load_area(key).unwrap();
                let spawn = area.spawn_points()[0];
                assert!(matches!(area.validate_move(spawn, spawn), MoveResult::Accepted(_)));
                assert_eq!(adapter.list_areas().len(), 3);
                area.info().fingerprint
            })
        })
        .collect();
    let prints: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(prints[0], prints[2]);
    assert_eq!(prints[1], prints[3]);
    assert_ne!(prints[0], prints[1]);

    // The source file is untouched by reading.
    drop(adapter);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    std::fs::remove_file(&path).unwrap();
}
