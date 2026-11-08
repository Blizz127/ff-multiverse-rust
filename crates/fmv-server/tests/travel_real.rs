//! Real-data check of walking between fields against the owner's own FF7 field archive. Never runs
//! by default: it needs the owner's `flevel.lgp` (read-only) and is `#[ignore]`d.
//!
//! ```text
//! FMV_FF7_FLEVEL=/path/to/flevel.lgp \
//!   cargo test -p fmv-server --test travel_real -- --ignored --nocapture
//! ```
//!
//! What it checks, in one process over real TCP: the server hosts `md1stin`, a raw client that holds
//! the same archive locally walks (a step at a time, every step validated with the adapter's own
//! `validate_move`, like the real client) from its spawn point to the middle of the gateway that
//! leads to `md1_1` and across it; the server offers `md1_1` (`AreaChange`: the arrival the field's
//! data names, settled on the target's walkmesh at the height of the named triangle), the client
//! answers from its own copy, and the server sends the snapshot of `md1_1` with its NPCs, directly
//! followed by the placement. Then the client walks to md1_1's own gateway back and ends in
//! `md1stin` again. Nothing from the archive is printed but counts, names, positions and timings.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use fmv_adapter_api::{AreaRuntime, GameAdapter, MoveResult};
use fmv_core::{AnimState, GameId, Pos};
use fmv_protocol::{read_msg, write_msg, Auth, ClientMsg, ServerMsg, PROTOCOL_VERSION};
use fmv_server::ff7::{build_tag, open_ff7, prewarm, Ff7Labels, Ff7Source, SharedFf7};
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

/// One walking step of at most `len` units from `cur` toward `aim`: the accepted candidate (of a
/// fan of headings around the straight line) that ends nearest the aim, as the headless bot does.
fn step_toward(area: &dyn AreaRuntime, cur: Pos, aim: Pos, len: f32) -> Option<Pos> {
    let heading = (aim.y - cur.y).atan2(aim.x - cur.x);
    let mut best: Option<(f32, Pos)> = None;
    for deg in [0.0_f32, 15.0, -15.0, 30.0, -30.0, 45.0, -45.0, 60.0, -60.0, 90.0, -90.0] {
        let h = heading + deg.to_radians();
        let want = Pos::new(cur.x + h.cos() * len, cur.y + h.sin() * len, cur.z);
        if let MoveResult::Accepted(p) = area.validate_move(cur, want) {
            let left = p.dist_xy(aim);
            if best.is_none_or(|(b, _)| left < b - 1e-3) {
                best = Some((left, p));
            }
        }
    }
    best.filter(|(left, _)| *left < cur.dist_xy(aim) - 1e-3).map(|(_, p)| p)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs the owner's real flevel.lgp: set FMV_FF7_FLEVEL"]
async fn walking_through_the_real_gateway_takes_a_client_from_md1stin_to_md1_1_and_back() {
    let Some(path) = std::env::var_os("FMV_FF7_FLEVEL").map(PathBuf::from) else {
        eprintln!("skipped: FMV_FF7_FLEVEL is not set");
        return;
    };
    let labels = Ff7Labels { edition: "real-data test".into(), ..Ff7Labels::default() };
    let opened = open_ff7(&Ff7Source::Flevel { path, labels }).expect("the archive opens");
    prewarm(&opened.adapter).await.expect("prewarm");

    // What the data says about the two fields.
    let started = Instant::now();
    let md1stin = opened.adapter.load_field("md1stin").expect("md1stin decodes");
    let md1_1 = opened.adapter.load_field("md1_1").expect("md1_1 decodes");
    eprintln!("both fields decoded in {:?}", started.elapsed());
    let exits = md1stin.exits();
    assert_eq!(exits.iter().map(|e| e.target.as_str()).collect::<Vec<_>>(), ["md1_1"], "md1stin's one gateway leads to md1_1");
    let exit = exits[0].clone();
    let back = md1_1.exits().into_iter().find(|e| e.target == "md1stin").expect("md1_1 has a gateway back to md1stin");
    let npcs = md1stin.npcs();
    eprintln!("md1stin: {} exit(s), {} NPC(s): {:?}", exits.len(), npcs.len(), npcs.iter().map(|n| n.entity_name.as_str()).collect::<Vec<_>>());
    let expected_arrival = md1_1.place_arrival(exit.arrival, exit.arrival_hint);
    assert!(expected_arrival.is_finite() && (expected_arrival.x, expected_arrival.y) == (exit.arrival.x, exit.arrival.y));

    // Host md1stin exactly the way the binary does (no battles: this is about walking).
    let config = ServerConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        area_key: "md1stin".into(),
        build_suffix: Some(build_tag()),
        ..ServerConfig::default()
    };
    let server = Server::bind(config, Box::new(SharedFf7(opened.adapter.clone()))).await.expect("bind");
    let addr = server.local_addr();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let serving = tokio::spawn(server.serve(async move {
        let _ = stopped.await;
    }));

    // A raw client joins md1stin.
    let mut c = timeout(STEP, TcpStream::connect(addr)).await.expect("connect timed out").expect("connect");
    write_msg(
        &mut c,
        &ClientMsg::Hello {
            protocol_version: PROTOCOL_VERSION,
            client_build: "travel-real-test".into(),
            auth: Auth::Register { display_name: "Walker".into() },
            tint: [9, 9, 9],
            appearance_ref: None,
        },
    )
    .await
    .unwrap();
    let (area, fingerprint, provenance) = until(&mut c, "AreaManifest", |m| match m {
        ServerMsg::AreaManifest { area, fingerprint, provenance, .. } => Some((area.clone(), *fingerprint, provenance.clone())),
        _ => None,
    })
    .await;
    assert_eq!(area.key, "md1stin");
    write_msg(&mut c, &ClientMsg::JoinArea { area, fingerprint, provenance }).await.unwrap();
    let (mut pos, snapshot_npcs) = until(&mut c, "AreaSnapshot", |m| match m {
        ServerMsg::AreaSnapshot { entities, npcs, .. } => Some((entities.first().expect("our entity").pos, npcs.clone())),
        _ => None,
    })
    .await;
    eprintln!("spawned at ({:.0}, {:.0}); the snapshot carries {} NPC(s)", pos.x, pos.y, snapshot_npcs.len());
    assert_eq!(snapshot_npcs.len(), npcs.len(), "the server announces the NPCs the adapter reports");

    // Walk to the middle of the gateway line, aiming a little past it, a step every 100 ms (the
    // server earns 90 units of budget per 100 ms; a step is 40).
    let mid = exit.midpoint();
    let (dx, dy) = (mid.x - pos.x, mid.y - pos.y);
    let d = dx.hypot(dy).max(1.0);
    let aim = Pos::new(mid.x + dx / d * 30.0, mid.y + dy / d * 30.0, mid.z);
    let mut seq = 0u32;
    let walk_started = Instant::now();
    let mut crossed = false;
    for _ in 0..600 {
        let Some(next) = step_toward(&md1stin, pos, aim, 40.0) else { panic!("stuck at ({:.0}, {:.0}) on the way to the gateway", pos.x, pos.y) };
        crossed = exit.is_crossed_by(pos, next);
        pos = next;
        seq += 1;
        sleep(Duration::from_millis(100)).await;
        write_msg(&mut c, &ClientMsg::MoveIntent { seq, pos, facing: 0.0, anim: AnimState::Walk }).await.unwrap();
        if crossed {
            break;
        }
    }
    assert!(crossed, "the walk never crossed the gateway");
    eprintln!("crossed the gateway after {seq} steps in {:?}", walk_started.elapsed());

    // The offer: md1_1, the arrival the data names, on the target's walkmesh.
    let offer_started = Instant::now();
    let (area, fingerprint, provenance, arrival, fixture) = until(&mut c, "AreaChange", |m| match m {
        ServerMsg::AreaChange { area, fingerprint, provenance, arrival, fixture, .. } => {
            Some((area.clone(), *fingerprint, provenance.clone(), *arrival, *fixture))
        }
        ServerMsg::MoveRejected { seq, corrected } => panic!("move {seq} was rejected, corrected to {corrected:?}"),
        _ => None,
    })
    .await;
    eprintln!("AreaChange arrived {:?} after the crossing step was sent", offer_started.elapsed());
    assert_eq!((area.game, area.key.as_str(), fixture), (GameId::Ff7, "md1_1", false));
    assert_eq!((arrival.x, arrival.y), (exit.arrival.x, exit.arrival.y), "the arrival is the one the field's data names");
    assert!((arrival.z - expected_arrival.z).abs() < 0.01, "settled at the height of the hinted triangle: {arrival:?} vs {expected_arrival:?}");
    let local = opened.adapter.list_areas().into_iter().find(|i| i.id == area).expect("the client holds md1_1");
    assert_eq!(fingerprint, local.fingerprint, "both sides hold the same field");
    write_msg(&mut c, &ClientMsg::JoinArea { area: area.clone(), fingerprint, provenance }).await.unwrap();
    let (entities, md1_1_npcs) = until(&mut c, "AreaSnapshot of md1_1", |m| match m {
        ServerMsg::AreaSnapshot { area: a, entities, npcs, .. } if a.key == "md1_1" => Some((entities.clone(), npcs.clone())),
        _ => None,
    })
    .await;
    assert_eq!(entities.len(), 1);
    assert_eq!((entities[0].pos.x, entities[0].pos.y), (arrival.x, arrival.y));
    assert_eq!(md1_1_npcs.len(), md1_1.npcs().len());
    match timeout(STEP, read_msg::<_, ServerMsg>(&mut c)).await.expect("a message after the snapshot").expect("read") {
        ServerMsg::InstancePlacement { placement } => assert_eq!(placement.key.area.key, "md1_1"),
        other => panic!("the placement follows the snapshot directly, got {other:?}"),
    }
    eprintln!("in md1_1 at ({:.0}, {:.0}, {:.0}); {} NPC(s) there", arrival.x, arrival.y, arrival.z, md1_1_npcs.len());

    // And back: walk to md1_1's own gateway to md1stin.
    let mut pos = arrival;
    let mid = back.midpoint();
    let (dx, dy) = (mid.x - pos.x, mid.y - pos.y);
    let d = dx.hypot(dy).max(1.0);
    let aim = Pos::new(mid.x + dx / d * 30.0, mid.y + dy / d * 30.0, mid.z);
    seq = 0;
    crossed = false;
    for _ in 0..600 {
        let Some(next) = step_toward(&md1_1, pos, aim, 80.0) else { panic!("stuck at ({:.0}, {:.0}) in md1_1", pos.x, pos.y) };
        crossed = back.is_crossed_by(pos, next);
        pos = next;
        seq += 1;
        sleep(Duration::from_millis(100)).await;
        write_msg(&mut c, &ClientMsg::MoveIntent { seq, pos, facing: 0.0, anim: AnimState::Walk }).await.unwrap();
        if crossed {
            break;
        }
    }
    assert!(crossed, "the walk in md1_1 never crossed the gateway back");
    let (area, fingerprint, provenance) = until(&mut c, "AreaChange back", |m| match m {
        ServerMsg::AreaChange { area, fingerprint, provenance, .. } => Some((area.clone(), *fingerprint, provenance.clone())),
        _ => None,
    })
    .await;
    assert_eq!(area.key, "md1stin");
    write_msg(&mut c, &ClientMsg::JoinArea { area, fingerprint, provenance }).await.unwrap();
    until(&mut c, "AreaSnapshot of md1stin", |m| matches!(m, ServerMsg::AreaSnapshot { area, .. } if area.key == "md1stin").then_some(())).await;
    eprintln!("back in md1stin");

    let _ = write_msg(&mut c, &ClientMsg::Leave).await;
    let _ = stop.send(());
    timeout(STEP * 3, serving).await.expect("server stops in time").expect("server task").expect("serve returns Ok");
}
