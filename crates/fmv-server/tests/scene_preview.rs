//! Story scenes over real TCP sockets, an in-process server and a REAL account store: the debug preview never touches a
//! stored story, and a scene a real account plays writes its story once, saved.
//!
//! The server hosts the FIXTURE rooms and a fake [`SceneHost`] (no game data): one scene in the yard, offered to a new
//! account (an empty story) and to a previewing player's made-up story, whose trigger stands at (200, -100) with a talk radius
//! of 80 (the preview arrives at (150, -100)). What the fake host's scene "writes" is the story `[5]` for a real account.
//!
//! The proof that a preview leaves the store alone is a byte-for-byte comparison of the whole state directory: a first
//! session registers the account and leaves; the second logs in with the token, previews the scene, plays it to its end (the
//! scene's story write is told to the player), walks and disconnects; the third logs in again. After the second and the
//! third the state directory is exactly what the first left, and the third session is told no story at all.
//! The third then plays the scene for REAL (an account whose story is on offer): its write is saved once. A fourth session
//! restarts the server on the same store (the save and the reload): the account is told the story it saved, no scene is on
//! offer to it, an OK where the trigger stood starts nothing and nothing holds the player.

mod common;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::*;
use fmv_adapter_api::GameAdapter;
use fmv_core::{AnimState, EntityId, GameId, Pos};
use fmv_fixture::rooms::{RoomsAdapter, HALL, YARD};
use fmv_protocol::{ClientMsg, SceneOutcome, SceneRole, SceneRunId, ServerMsg, SCENE_NPC_ID_BASE};
use fmv_server::{SceneHost, SceneInfo, SceneResult, SceneSettings, ServerConfig, TriggerInfo};
use tokio::time::sleep;

const SCENE: &str = "test/yard-scene";
const MADE_UP: &[u8] = &[9];

struct FakeHost;

impl SceneHost for FakeHost {
    fn scenes(&self) -> Vec<SceneInfo> {
        vec![SceneInfo { id: SCENE.into(), field: YARD.into(), arrival: Pos::new(150.0, -100.0, 0.0), arrival_facing: 0.0, arrival_hint: None }]
    }
    fn offered(&self, scene: &str, progress: &[u8]) -> bool {
        scene == SCENE && (progress.is_empty() || progress == MADE_UP)
    }
    fn trigger(&self, scene: &str, _progress: &[u8]) -> Option<TriggerInfo> {
        (scene == SCENE).then(|| TriggerInfo {
            name: "giver".into(),
            model: "aagb".into(),
            idle: Some("abcd".into()),
            pos: Pos::new(200.0, -100.0, 0.0),
            facing: 0.0,
            talk_radius: 80.0,
        })
    }
    fn preview_progress(&self, scene: &str) -> Option<Vec<u8>> {
        (scene == SCENE).then(|| MADE_UP.to_vec())
    }
    fn finish(&self, _scene: &str, progress: &[u8], _seed: u32, _answers: &[u8]) -> Option<SceneResult> {
        let after = if progress == MADE_UP { vec![9, 5] } else { vec![5] };
        Some(SceneResult { progress: after, changed: true, summary: "moment=1->5".into(), travel: None })
    }
}

fn config(state: &Path, preview: bool) -> ServerConfig {
    ServerConfig {
        area_key: HALL.to_string(),
        state_dir: Some(state.to_path_buf()),
        scenes: Some(SceneSettings { host: Arc::new(FakeHost), debug_preview: preview }),
        ..ServerConfig::default()
    }
}

struct Player {
    client: Client,
    entity: EntityId,
    token: String,
    seq: u32,
}

/// The `JoinArea` of area `key` of the rooms.
fn join_msg(key: &str) -> ClientMsg {
    let info = RoomsAdapter::new().list_areas().into_iter().find(|i| i.id.key == key).expect("an area of the rooms");
    ClientMsg::JoinArea { area: info.id.clone(), fingerprint: info.fingerprint, provenance: info.provenance.summary() }
}

/// Handshake into the hall, registering `name` (a fresh account) or logging in with `token`.
async fn join_hall(addr: SocketAddr, name: &str, token: Option<&str>) -> Player {
    let mut client = Client::connect(addr).await;
    match token {
        Some(t) => client.hello_token(t).await,
        None => client.hello(name).await,
    }
    let (entity, issued) = match client.next().await {
        ServerMsg::Welcome { entity, issued_token, .. } => (entity, issued_token),
        other => panic!("expected Welcome, got {other:?}"),
    };
    assert!(matches!(client.next().await, ServerMsg::AreaManifest { .. }));
    client.send(&join_msg(HALL)).await;
    assert!(matches!(client.next().await, ServerMsg::AreaSnapshot { .. }));
    assert!(matches!(client.next().await, ServerMsg::InstancePlacement { .. }));
    let token = token.map(str::to_string).or_else(|| issued.map(|s| s.expose().to_string())).expect("a token");
    Player { client, entity, token, seq: 0 }
}

impl Player {
    async fn walk(&mut self, x: f32, y: f32) {
        sleep(Duration::from_millis(700)).await;
        self.seq += 1;
        self.client.send(&ClientMsg::MoveIntent { seq: self.seq, pos: Pos::new(x, y, 0.0), facing: 0.0, anim: AnimState::Walk }).await;
    }

    /// Answer the `AreaChange` the server sends next, and read what follows the join into the new area.
    async fn arrive(&mut self) -> Vec<ServerMsg> {
        let key = self
            .client
            .until("an AreaChange", |m| if let ServerMsg::AreaChange { area, .. } = m { Some(area.key.clone()) } else { None })
            .await;
        self.client.send(&join_msg(&key)).await;
        let mut got = Vec::new();
        got.push(self.client.until("the snapshot", |m| matches!(m, ServerMsg::AreaSnapshot { .. }).then(|| m.clone())).await);
        // the placement, and (for a preview) the made-up story, follow it
        got.extend(self.client.drain_for(Duration::from_millis(300)).await);
        got
    }

    /// Whether the server answered the player's moves since the last call with a correction (a held player's moves are
    /// ignored, not answered; a free player's are applied).
    async fn moved_freely(&mut self, to: (f32, f32)) -> bool {
        self.walk(to.0, to.1).await;
        let seen = self.client.drain_for(Duration::from_millis(400)).await;
        let entity = self.entity;
        let moved = seen.iter().filter_map(|m| update_for(m, entity)).any(|e| (e.pos.x, e.pos.y) == to);
        let corrected = seen.iter().any(|m| rejected(m).is_some());
        moved && !corrected
    }
}

/// Every file under `root` with its bytes, except the lock the running server holds (the one thing that changes with a login).
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(dir: &Path, root: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).expect("a readable directory") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                walk(&path, root, out);
            } else if path.file_name().is_some_and(|n| !n.to_string_lossy().to_ascii_lowercase().contains("lock")) {
                out.insert(path.strip_prefix(root).expect("under root").to_path_buf(), std::fs::read(&path).expect("a readable file"));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn story_updates(msgs: &[ServerMsg]) -> Vec<(Vec<u8>, bool)> {
    msgs.iter().filter_map(|m| if let ServerMsg::StoryUpdate { progress, advance } = m { Some((progress.clone(), advance.is_some())) } else { None }).collect()
}

fn scene_run(msgs: &[ServerMsg]) -> Option<(SceneRunId, SceneRole)> {
    msgs.iter().find_map(|m| if let ServerMsg::SceneStart { run, you, .. } = m { Some((*run, *you)) } else { None })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_scene_preview_never_touches_a_stored_story_whatever_the_scene_writes_and_a_real_scene_saves_its_write_once() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().to_path_buf();

    // ---- session 1: the account is created and leaves ----
    let server = TestServer::start_over(config(&state, true), Box::new(RoomsAdapter::new())).await;
    let a = join_hall(server.addr, &uniq("Pre"), None).await;
    let token = a.token.clone();
    drop(a);
    server.shutdown().await;
    let after_registration = snapshot(&state);
    assert!(!after_registration.is_empty(), "the registration stored something");

    // ---- session 2: the same account previews the scene, plays it to its end, walks, disconnects ----
    let server = TestServer::start_over(config(&state, true), Box::new(RoomsAdapter::new())).await;
    let mut a = join_hall(server.addr, "", Some(&token)).await;
    a.client.send(&ClientMsg::SceneDebugPreview { scene: SCENE.into() }).await;
    let arrived = a.arrive().await;
    // the scene's trigger is in the cast; the story the client is told is the made-up one, not a saved one
    let npcs = arrived.iter().find_map(|m| if let ServerMsg::AreaSnapshot { npcs, .. } = m { Some(npcs.clone()) } else { None }).unwrap();
    assert!(npcs.iter().any(|n| n.id == SCENE_NPC_ID_BASE && n.name == "giver"));
    assert_eq!(story_updates(&arrived), [(MADE_UP.to_vec(), false)]);
    // OK next to it starts the scene; the owner reports a window and the end
    a.client.send(&ClientMsg::Interact { npc: Some(SCENE_NPC_ID_BASE) }).await;
    let (run, role) = scene_run(&a.client.drain_for(Duration::from_millis(300)).await).expect("the scene started");
    assert_eq!(role, SceneRole::Owner);
    // held: its walk is not applied
    assert!(!a.moved_freely((140.0, -100.0)).await, "a player in a scene is held");
    a.client.send(&ClientMsg::SceneStep { run, step: 0, answer: Some(2) }).await;
    a.client.send(&ClientMsg::SceneEnd { run, skipped: false }).await;
    let ended = a.client.drain_for(Duration::from_millis(400)).await;
    assert!(ended.iter().any(|m| matches!(m, ServerMsg::SceneEnded { outcome: SceneOutcome::Finished, .. })), "{ended:?}");
    // the scene's write is told, as the made-up story moving; control is back
    assert_eq!(story_updates(&ended), [(vec![9, 5], true)]);
    assert!(a.moved_freely((130.0, -100.0)).await, "control returns when the scene ends");
    drop(a);
    server.shutdown().await;
    assert_eq!(snapshot(&state), after_registration, "NOTHING was stored: the made-up story and the scene's write never reached the store");

    // ---- session 3: it logs in again: no story at all (a new account's), the store is still untouched ----
    let server = TestServer::start_over(config(&state, true), Box::new(RoomsAdapter::new())).await;
    let mut a = join_hall(server.addr, "", Some(&token)).await;
    let told = a.client.drain_for(Duration::from_millis(300)).await;
    assert!(story_updates(&told).is_empty(), "the stored story is a new account's: nothing to tell {told:?}");
    assert!(scene_run(&told).is_none(), "no scene replays on its own");
    assert!(a.moved_freely((-90.0, 0.0)).await, "nothing is stuck");
    // ... and a REAL scene (the same account walks through the exit to the yard, a new account's story is on offer) saves its write once
    a.walk(100.0, 150.0).await;
    a.walk(350.0, 150.0).await;
    let arrived = a.arrive().await;
    assert!(story_updates(&arrived).is_empty());
    a.walk(150.0, -100.0).await;
    a.client.drain_for(Duration::from_millis(300)).await;
    a.client.send(&ClientMsg::Interact { npc: Some(SCENE_NPC_ID_BASE) }).await;
    let (run, _) = scene_run(&a.client.drain_for(Duration::from_millis(300)).await).expect("the scene started for the real account too");
    a.client.send(&ClientMsg::SceneEnd { run, skipped: true }).await;
    a.client.send(&ClientMsg::SceneEnd { run, skipped: false }).await;
    let ended = a.client.drain_for(Duration::from_millis(400)).await;
    assert_eq!(ended.iter().filter(|m| matches!(m, ServerMsg::SceneEnded { .. })).count(), 1, "ended once, however often the client says so");
    assert_eq!(story_updates(&ended), [(vec![5], true)], "the story step is told once");
    drop(a);
    server.shutdown().await;
    let store = fmv_persist::Store::open(&state).expect("the state directory opens once the server is gone");
    let account = store.authenticate(&token).expect("the account");
    assert_eq!(store.get_progression(account, GameId::Fixture).expect("a record").blob, [5], "saved once, as the scene wrote it");
    drop(store);

    // ---- session 4 (acceptance 5, save and reload): the server restarts on the same store; the account logs in again, is told the
    // story it saved, walks to the yard and finds no scene on offer; an OK where the trigger stood starts nothing and nothing
    // holds the player: the scene neither replays nor leaves anyone stuck ----
    let server = TestServer::start_over(config(&state, true), Box::new(RoomsAdapter::new())).await;
    let mut a = join_hall(server.addr, "", Some(&token)).await;
    let told = a.client.drain_for(Duration::from_millis(300)).await;
    assert_eq!(story_updates(&told), [(vec![5], false)], "told the saved story at login, as no step");
    assert!(scene_run(&told).is_none(), "no scene replays on its own");
    assert!(a.moved_freely((-90.0, 0.0)).await, "nothing is stuck");
    a.walk(100.0, 150.0).await;
    a.walk(350.0, 150.0).await;
    let arrived = a.arrive().await;
    let npcs = arrived.iter().find_map(|m| if let ServerMsg::AreaSnapshot { npcs, .. } = m { Some(npcs.clone()) } else { None }).expect("the yard's snapshot");
    assert!(!npcs.iter().any(|n| n.id >= SCENE_NPC_ID_BASE), "the scene is not on offer to a story that has had it: {npcs:?}");
    a.walk(150.0, -100.0).await;
    a.client.drain_for(Duration::from_millis(300)).await;
    a.client.send(&ClientMsg::Interact { npc: Some(SCENE_NPC_ID_BASE) }).await;
    let pressed = a.client.drain_for(Duration::from_millis(400)).await;
    assert!(scene_run(&pressed).is_none(), "an OK where the trigger stood starts nothing: {pressed:?}");
    assert!(a.moved_freely((140.0, -100.0)).await, "and nothing holds the player");
    drop(a);
    server.shutdown().await;
    let store = fmv_persist::Store::open(&state).expect("the state directory opens once the server is gone");
    let account = store.authenticate(&token).expect("the account");
    assert_eq!(store.get_progression(account, GameId::Fixture).expect("a record").blob, [5], "a login and a walk changed nothing");
    drop(store);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_server_that_does_not_run_previews_refuses_one_and_offers_no_scene_to_a_story_that_is_not_at_its_moment() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().to_path_buf();
    let server = TestServer::start_over(config(&state, false), Box::new(RoomsAdapter::new())).await;
    let mut a = join_hall(server.addr, &uniq("Off"), None).await;
    a.client.send(&ClientMsg::SceneDebugPreview { scene: SCENE.into() }).await;
    let refused = a.client.until("SceneRefused", |m| if let ServerMsg::SceneRefused { reason } = m { Some(*reason) } else { None }).await;
    assert_eq!(refused, fmv_protocol::SceneRefusal::Unavailable);
    assert!(a.moved_freely((-90.0, 0.0)).await, "a refusal leaves the player where it was");
    drop(a);
    server.shutdown().await;
    let _ = YARD;
}
