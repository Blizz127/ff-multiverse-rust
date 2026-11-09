//! Real-data check against the owner's own FF7 field archive. Never runs by default: it needs
//! the owner's `flevel.lgp` (read-only) and is `#[ignore]`d.
//!
//! ```text
//! FMV_FF7_FLEVEL=/path/to/flevel.lgp \
//!   cargo test -p fmv-server --test ff7_real -- --ignored --nocapture
//! ```
//!
//! `FMV_FF7_FLEVEL` may point at the original file or at a library blob; both are opened
//! read-only. `FMV_FF7_AREA` overrides the field (default `md1stin`). Nothing from the archive is
//! asserted byte-for-byte or printed: the test prints counts, extents and timings, so it doubles
//! as a probe of how the decoders do on real data.
//!
//! What it checks end to end, in one process over real TCP: the archive indexes, the field
//! decodes, the server announces it as `Ff7/<area>` with `fixture=false` and a provenance summary
//! without paths, a raw client can join with the announced fingerprint, a short walk that the
//! adapter accepts is applied by the server, and a step that the adapter refuses is rejected with
//! a correction back onto the walkable surface.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use fmv_adapter_api::{AreaRuntime, MoveResult};
use fmv_core::{AnimState, AreaId, GameId, Pos};
use fmv_protocol::{read_msg, write_msg, Auth, ClientMsg, ServerMsg, PROTOCOL_VERSION};
use fmv_server::ff7::{build_tag, open_ff7, prewarm, Ff7Labels, Ff7Source, SharedFf7, DEFAULT_FF7_AREA};
use fmv_server::{Server, ServerConfig};
use tokio::net::TcpStream;
use tokio::time::{sleep, timeout};

const STEP: Duration = Duration::from_secs(10);

/// Wait for a message matching `pick`, skipping everything else (joins, updates, pongs).
async fn until<T>(stream: &mut TcpStream, what: &str, mut pick: impl FnMut(&ServerMsg) -> Option<T>) -> T {
    loop {
        let msg = timeout(STEP, read_msg::<_, ServerMsg>(stream))
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
            .unwrap_or_else(|e| panic!("connection ended while waiting for {what}: {e}"));
        if let Some(found) = pick(&msg) {
            return found;
        }
    }
}

fn extent(tris: &[[Pos; 3]], axis: impl Fn(&Pos) -> f32) -> (f32, f32) {
    let mut lo = f32::MAX;
    let mut hi = f32::MIN;
    for p in tris.iter().flatten() {
        lo = lo.min(axis(p));
        hi = hi.max(axis(p));
    }
    (lo, hi)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs the owner's real flevel.lgp: set FMV_FF7_FLEVEL"]
async fn the_real_field_is_hosted_and_the_walkmesh_is_the_authority() {
    let Some(path) = std::env::var_os("FMV_FF7_FLEVEL").map(PathBuf::from) else {
        eprintln!("skipped: FMV_FF7_FLEVEL is not set");
        return;
    };
    let key = std::env::var("FMV_FF7_AREA").unwrap_or_else(|_| DEFAULT_FF7_AREA.to_string());

    // 1. Open and index. The labels only have to agree between this server and this client.
    let labels = Ff7Labels { edition: "real-data test".into(), ..Ff7Labels::default() };
    let opened = open_ff7(&Ff7Source::Flevel { path, labels }).expect("the archive opens");
    let report = prewarm(&opened.adapter).await.expect("prewarm");
    eprintln!("indexed: {} areas, {} entries skipped, in {:?}", report.areas, report.skipped, report.elapsed);
    for s in opened.adapter.skipped_entries().iter().take(8) {
        eprintln!("  skipped {:?}: {}", s.name, s.reason);
    }
    assert!(report.areas > 100, "a whole game has hundreds of fields, the index found {}", report.areas);

    // 2. Decode the field and print what the decoders made of it.
    let started = Instant::now();
    let field = opened.adapter.load_field(&key).unwrap_or_else(|e| panic!("field {key} does not decode: {e}"));
    eprintln!("decoded {key} in {:?}", started.elapsed());
    let tris = field.walk_triangles();
    let (bounds, spawns, max_speed) = (field.bounds(), field.spawn_points(), field.max_speed());
    eprintln!("walkmesh: {} triangles; link report {:?}", tris.len(), field.link_report());
    let (x, y, z) = (extent(&tris, |p| p.x), extent(&tris, |p| p.y), extent(&tris, |p| p.z));
    eprintln!("extents: x {x:?} y {y:?} z {z:?} (bounds {:?} .. {:?})", bounds.min, bounds.max);
    eprintln!("spawn points: {spawns:?}; max_speed {max_speed} units/s");
    eprintln!("cameras decoded: {:?}", field.cameras().map(|c| c.len()));
    assert!(!tris.is_empty() && !spawns.is_empty());
    assert!(bounds.min.is_finite() && bounds.max.is_finite());

    // 3. Host it exactly the way the binary does.
    let config = ServerConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        area_key: key.clone(),
        build_suffix: Some(build_tag()),
        ..ServerConfig::default()
    };
    let server = Server::bind(config, Box::new(SharedFf7(opened.adapter.clone()))).await.expect("bind");
    let addr = server.local_addr();
    let ready = server.ready_line();
    eprintln!("{ready}");
    assert!(ready.contains(&format!("area=Ff7/{key} fixture=false")), "{ready}");
    assert!(ready.contains("+fmv-ff7/"), "the adapter version is part of the build: {ready}");
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let serving = tokio::spawn(server.serve(async move {
        let _ = stopped.await;
    }));

    // 4. A raw client: Hello -> Welcome + manifest -> JoinArea -> snapshot.
    let mut c = timeout(STEP, TcpStream::connect(addr)).await.expect("connect timed out").expect("connect");
    write_msg(
        &mut c,
        &ClientMsg::Hello {
            protocol_version: PROTOCOL_VERSION,
            client_build: "ff7-real-test".into(),
            auth: Auth::Register { display_name: "Probe".into() },
            character: fmv_protocol::NewCharacter::with_tint([9, 9, 9]),
        },
    )
    .await
    .unwrap();
    let (area, fingerprint, provenance) = until(&mut c, "AreaManifest", |m| match m {
        ServerMsg::AreaManifest { area, fingerprint, fixture, provenance, display_name, .. } => {
            assert!(!fixture, "real content must not be announced as FIXTURE");
            assert_eq!(provenance.game, GameId::Ff7);
            assert!(!display_name.contains("FIXTURE"), "{display_name}");
            let wire = format!("{provenance:?}");
            assert!(!wire.contains("flevel") && !wire.contains('/'), "no path may reach the wire: {wire}");
            Some((area.clone(), *fingerprint, provenance.clone()))
        }
        _ => None,
    })
    .await;
    assert_eq!(area, AreaId { game: GameId::Ff7, key: key.clone() });
    assert_eq!(fingerprint, field.info().fingerprint, "the announced fingerprint is the adapter's");
    write_msg(&mut c, &ClientMsg::JoinArea { area, fingerprint, provenance }).await.unwrap();
    let start = until(&mut c, "AreaSnapshot", |m| match m {
        ServerMsg::AreaSnapshot { entities, .. } => Some(entities.first().expect("our own entity").pos),
        _ => None,
    })
    .await;
    eprintln!("spawned at {start:?}");
    assert!(spawns.contains(&start), "the server spawns players on the adapter's spawn points");

    // Candidate destinations on a ring around `center`, classified by the adapter itself.
    let ring = |center: Pos, radius: f32| -> Vec<Pos> {
        (0..32)
            .map(|i| {
                let a = i as f32 / 32.0 * std::f32::consts::TAU;
                Pos::new(center.x + radius * a.cos(), center.y + radius * a.sin(), center.z)
            })
            .collect()
    };
    let walkable = ring(start, 40.0)
        .into_iter()
        .find_map(|to| match field.validate_move(start, to) {
            MoveResult::Accepted(p) => Some(p),
            MoveResult::Corrected(_) => None,
        })
        .expect("some 40-unit step from a spawn point is walkable");

    // 5. The server's travel budget starts empty: let it earn the distance, then walk.
    sleep(Duration::from_millis(400)).await;
    write_msg(&mut c, &ClientMsg::MoveIntent { seq: 1, pos: walkable, facing: 0.5, anim: AnimState::Walk }).await.unwrap();
    let moved = until(&mut c, "the EntityUpdate of our own step", |m| match m {
        ServerMsg::EntityUpdate { entities, .. } => entities.iter().find(|e| e.anim == AnimState::Walk).map(|e| e.pos),
        ServerMsg::MoveRejected { seq: 1, corrected } => panic!("the walkable step was rejected, corrected to {corrected:?}"),
        _ => None,
    })
    .await;
    assert!((moved.x - walkable.x).abs() < 0.5 && (moved.y - walkable.y).abs() < 0.5, "{moved:?} vs {walkable:?}");

    // 6. A step the adapter refuses is refused by the server too, and the entity stays where it
    //    was. The refused destination is chosen from where the server now holds us. (A field
    //    with no wall within reach of the spawn cannot show this.)
    let refused = [40.0, 120.0, 300.0]
        .into_iter()
        .find_map(|r| ring(moved, r).into_iter().find(|to| matches!(field.validate_move(moved, *to), MoveResult::Corrected(_))));
    eprintln!("walked to {moved:?}; a step the adapter refuses from there: {refused:?}");
    if let Some(off) = refused {
        sleep(Duration::from_millis(600)).await; // earn the distance again (budget cap: 450 units)
        write_msg(&mut c, &ClientMsg::MoveIntent { seq: 2, pos: off, facing: 0.0, anim: AnimState::Walk }).await.unwrap();
        let corrected = until(&mut c, "MoveRejected", |m| match m {
            ServerMsg::MoveRejected { seq: 2, corrected } => Some(*corrected),
            _ => None,
        })
        .await;
        assert!(corrected.is_finite());
        assert!(corrected.dist_xy(moved) < 1.0, "the entity must stay where it was: {corrected:?} vs {moved:?}");
    } else {
        eprintln!("note: no refused step within 300 units of the position, so the off-mesh rejection was not exercised");
    }

    let _ = write_msg(&mut c, &ClientMsg::Leave).await;
    let _ = stop.send(());
    timeout(STEP * 3, serving).await.expect("server stops in time").expect("server task").expect("serve returns Ok");
}
