//! The proof scene (Cloud and Tifa at the bar of 7th Heaven, field `mds7pb_1`) hosted by a REAL server over the owner's own field
//! archive, read-only, with a raw client over a real socket. Never runs by default: it needs the owner's `flevel.lgp` and is
//! `#[ignore]`d.
//!
//! ```text
//! FMV_FF7_FLEVEL=/path/to/flevel.lgp cargo test -p fmv-server --test scene_real -- --ignored --nocapture
//! ```
//!
//! The server runs with `--debug-scene-preview` semantics (`SceneSettings::debug_preview`), in a private temporary state
//! directory. What it proves, end to end on the real field: the preview puts the player in `mds7pb_1` at the scene's own moment
//! (told as a made-up story), the field's cast now holds the scene's trigger (Tifa, under the scene NPC id) where her stage
//! script stands her, an OK next to her starts the scene (acceptance 1), the server holds the player until the owner ends it,
//! the owner's reported steps and answers are accepted in order, the server's own headless run of the scene (with those
//! answers) finishes, and control returns (acceptance 4). Nothing from the archive is printed but counts and ids.

use std::path::PathBuf;
use std::time::Duration;

use fmv_core::{AnimState, GameId, Pos};
use fmv_ff7::script::scene::{proof_scene, PROOF_SCENE_ID};
use fmv_protocol::{read_msg, write_msg, Auth, ClientMsg, SceneOutcome, SceneRole, ServerMsg, PROTOCOL_VERSION, SCENE_NPC_ID_BASE};
use fmv_server::ff7::{build_tag, open_ff7, prewarm, Ff7Labels, Ff7Source, SharedFf7};
use fmv_server::scene_host::Ff7SceneHost;
use fmv_server::{SceneHost, SceneSettings, Server, ServerConfig};
use tokio::net::TcpStream;
use tokio::time::{sleep, timeout};

const STEP: Duration = Duration::from_secs(15);

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

/// Everything that arrives in the next `quiet` of silence.
async fn drain(stream: &mut TcpStream, quiet: Duration) -> Vec<ServerMsg> {
    let mut out = Vec::new();
    while let Ok(Ok(msg)) = timeout(quiet, read_msg::<_, ServerMsg>(stream)).await {
        out.push(msg);
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs the owner's real flevel.lgp: set FMV_FF7_FLEVEL"]
async fn the_proof_scene_starts_from_its_trigger_on_the_real_field_holds_the_player_and_releases_it() {
    let Some(path) = std::env::var_os("FMV_FF7_FLEVEL").map(PathBuf::from) else {
        eprintln!("skipped: FMV_FF7_FLEVEL is not set");
        return;
    };
    let def = proof_scene();
    let labels = Ff7Labels { edition: "real-data test".into(), ..Ff7Labels::default() };
    let opened = open_ff7(&Ff7Source::Flevel { path, labels }).expect("the archive opens");
    prewarm(&opened.adapter).await.expect("prewarm");

    // The host over the real field, asked directly first.
    let host = Ff7SceneHost::new(opened.adapter.clone());
    let scenes = host.scenes();
    assert_eq!(scenes.len(), 1);
    assert_eq!((scenes[0].id.as_str(), scenes[0].field.as_str()), (PROOF_SCENE_ID, "mds7pb_1"));
    let moment_72 = host.preview_progress(PROOF_SCENE_ID).expect("a made-up story");
    assert_eq!(moment_72, [1, 72, 0], "the scene's own moment");
    assert!(host.offered(PROOF_SCENE_ID, &moment_72));
    assert!(!host.offered(PROOF_SCENE_ID, &[1, 71, 0]) && !host.offered(PROOF_SCENE_ID, &[]), "a new account (moment 1) is not offered it");
    assert!(!host.offered(PROOF_SCENE_ID, &[7, 7]), "an unreadable story is offered nothing");
    let trigger = host.trigger(PROOF_SCENE_ID, &moment_72).expect("Tifa is found where her stage script puts her");
    assert_eq!((trigger.name.as_str(), trigger.model.as_str()), ("tifa", "aagb"));
    assert_eq!((trigger.pos.x, trigger.pos.y), (-268.0, -105.0));
    // The server's own headless run, with the answers the owner would report, finishes and writes exactly the bar's mark
    // (`Var[3][212]` bit 2: the story model carries it so that the conversation is had once per story): the moment stays 72, the
    // story now holds the mark, applying the run to the story that holds it changes nothing (once), and the scene is no longer
    // offered to it (acceptance 5: nothing replays after the story is saved and read back).
    let finished = host.finish(PROOF_SCENE_ID, &moment_72, 1, &[2]).expect("the scene runs");
    assert!(finished.changed, "the proof scene writes the bar's mark: {finished:?}");
    assert_eq!(finished.progress, [2, 72, 0, 1, 3, 212, 4], "moment 72 and the mark (var 3/212 = 0b100), the one way to write it");
    assert!(!host.offered(PROOF_SCENE_ID, &finished.progress), "a story that has had the conversation is not offered it again");
    let again = host.finish(PROOF_SCENE_ID, &finished.progress, 1, &[2]).expect("the scene runs again for a story that has it");
    assert!(!again.changed && again.progress == finished.progress, "its write is applied once: {again:?}");

    // A real server over the same archive, entry field md1stin, previews on, a private state directory.
    let state = tempfile::tempdir().unwrap();
    let config = ServerConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        area_key: "md1stin".to_string(),
        build_suffix: Some(build_tag()),
        state_dir: Some(state.path().to_path_buf()),
        scenes: Some(SceneSettings { host: std::sync::Arc::new(Ff7SceneHost::new(opened.adapter.clone())), debug_preview: true }),
        ..ServerConfig::default()
    };
    let server = Server::bind(config, Box::new(SharedFf7(opened.adapter.clone()))).await.expect("bind");
    let addr = server.local_addr();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let serving = tokio::spawn(server.serve(async move {
        let _ = stopped.await;
    }));

    let mut c = timeout(STEP, TcpStream::connect(addr)).await.expect("connect timed out").expect("connect");
    write_msg(
        &mut c,
        &ClientMsg::Hello {
            protocol_version: PROTOCOL_VERSION,
            client_build: "scene-real-test".into(),
            auth: Auth::Register { display_name: "Witness".into() },
            character: fmv_protocol::NewCharacter::with_tint([9, 9, 9]),
        },
    )
    .await
    .unwrap();
    let entity = until(&mut c, "Welcome", |m| if let ServerMsg::Welcome { entity, .. } = m { Some(*entity) } else { None }).await;
    let (area, fingerprint, provenance) =
        until(&mut c, "AreaManifest", |m| if let ServerMsg::AreaManifest { area, fingerprint, provenance, .. } = m { Some((area.clone(), *fingerprint, provenance.clone())) } else { None })
            .await;
    write_msg(&mut c, &ClientMsg::JoinArea { area, fingerprint, provenance }).await.unwrap();
    until(&mut c, "AreaSnapshot", |m| matches!(m, ServerMsg::AreaSnapshot { .. }).then_some(())).await;
    drain(&mut c, Duration::from_millis(300)).await;

    // The preview: sent to 7th Heaven's bar floor at the scene's own moment.
    write_msg(&mut c, &ClientMsg::SceneDebugPreview { scene: PROOF_SCENE_ID.into() }).await.unwrap();
    let (area, fingerprint, provenance, arrival) = until(&mut c, "AreaChange", |m| {
        if let ServerMsg::AreaChange { area, fingerprint, provenance, arrival, .. } = m {
            Some((area.clone(), *fingerprint, provenance.clone(), *arrival))
        } else {
            None
        }
    })
    .await;
    assert_eq!(area.key, "mds7pb_1");
    assert_eq!((arrival.x.round(), arrival.y.round()), (f32::from(def.arrival.x), f32::from(def.arrival.y)), "where the scene says");
    write_msg(&mut c, &ClientMsg::JoinArea { area, fingerprint, provenance }).await.unwrap();
    let arrived = drain(&mut c, Duration::from_millis(500)).await;
    let npcs = arrived.iter().find_map(|m| if let ServerMsg::AreaSnapshot { npcs, .. } = m { Some(npcs.clone()) } else { None }).expect("a snapshot of the bar");
    let tifa = npcs.iter().find(|n| n.id >= SCENE_NPC_ID_BASE).expect("the scene's trigger is in the cast");
    assert_eq!((tifa.id, tifa.name.as_str(), tifa.model.as_str()), (SCENE_NPC_ID_BASE, "tifa", "aagb"));
    assert_eq!((tifa.pos.x, tifa.pos.y), (-268.0, -105.0));
    let told: Vec<&Vec<u8>> = arrived.iter().filter_map(|m| if let ServerMsg::StoryUpdate { progress, .. } = m { Some(progress) } else { None }).collect();
    assert_eq!(told, [&moment_72], "told the made-up story, moment 72");

    // OK next to Tifa starts the scene (acceptance 1): the owner, alone, with the witness spot 0.
    write_msg(&mut c, &ClientMsg::Interact { npc: Some(SCENE_NPC_ID_BASE) }).await.unwrap();
    let started = until(&mut c, "SceneStart", |m| {
        if let ServerMsg::SceneStart { run, scene, you, participants, progress, owner, .. } = m {
            Some((*run, scene.clone(), *you, participants.clone(), progress.clone(), *owner))
        } else {
            None
        }
    })
    .await;
    let (run, scene, role, participants, progress, owner) = started;
    assert_eq!((scene.as_str(), role, owner), (PROOF_SCENE_ID, SceneRole::Owner, entity));
    assert_eq!(participants.iter().map(|p| (p.entity, p.spot)).collect::<Vec<_>>(), [(entity, 0)]);
    assert_eq!(progress, moment_72);

    // Held until the owner ends it: a walk is not applied.
    sleep(Duration::from_millis(700)).await;
    write_msg(&mut c, &ClientMsg::MoveIntent { seq: 1, pos: Pos::new(-115.0, -100.0, 0.0), facing: 0.0, anim: AnimState::Walk }).await.unwrap();
    let seen = drain(&mut c, Duration::from_millis(400)).await;
    assert!(
        !seen.iter().any(|m| matches!(m, ServerMsg::EntityUpdate { entities, .. } if entities.iter().any(|e| e.id == entity && (e.pos.x - -115.0).abs() < 1.0))),
        "a player in a scene is held"
    );

    // The owner plays it: eleven windows, the question answered 'something hard' (line 2), then the end.
    for step in 0..11u32 {
        let answer = (step == 1).then_some(2u8);
        write_msg(&mut c, &ClientMsg::SceneStep { run, step, answer }).await.unwrap();
    }
    write_msg(&mut c, &ClientMsg::SceneEnd { run, skipped: false }).await.unwrap();
    let ended = until(&mut c, "SceneEnded", |m| if let ServerMsg::SceneEnded { outcome, .. } = m { Some(*outcome) } else { None }).await;
    assert_eq!(ended, SceneOutcome::Finished);

    // Control returns (acceptance 4): the same walk is applied now.
    sleep(Duration::from_millis(700)).await;
    write_msg(&mut c, &ClientMsg::MoveIntent { seq: 2, pos: Pos::new(-115.0, -100.0, 0.0), facing: 0.0, anim: AnimState::Walk }).await.unwrap();
    let seen = drain(&mut c, Duration::from_millis(600)).await;
    assert!(
        seen.iter().any(|m| matches!(m, ServerMsg::EntityUpdate { entities, .. } if entities.iter().any(|e| e.id == entity && (e.pos.x - -115.0).abs() < 1.0))),
        "control is back: {seen:?}"
    );
    assert!(!seen.iter().any(|m| matches!(m, ServerMsg::MoveRejected { .. })));

    // Nothing of the preview was stored: the account has no story record at all (a new account's).
    drop(c);
    let _ = stop.send(());
    timeout(STEP * 2, serving).await.expect("the server stops").expect("no panic").expect("serve returns Ok");
    let store = fmv_persist::Store::open(state.path()).expect("the state directory opens");
    let account = store.find_account_by_name("Witness").expect("the account");
    let record = store.get_progression(account, GameId::Ff7).expect("a record or the default");
    assert!(record.blob.is_empty(), "NOTHING of a preview is stored: {:?}", record.blob);
}
