//! The headless loop with a `--scene-*` flag against a SCRIPTED FAKE SERVER on a real socket: the whole bot, from the flags
//! to the exit code, with the game's scene replaced by the invented one of `crate::scene::tests` (no game data).
//!
//! The fake server keeps the real server's side of the contract that matters here: a preview request is answered by the
//! snapshot of the scene's field (which holds the trigger), an OK on the trigger starts the run (`SceneStart`), the owner's
//! steps and end are recorded, `SceneEnd` is answered by `SceneEnded`, and a move that comes while the player is held is
//! answered with nothing while one that comes after is echoed to its mover. What the bot printed is read from the sink
//! `print_scene_lines` keeps in a test build (the same lines go to stdout in a real run).

use super::*;
use crate::audio::GameAudio;
use crate::net::{NetConfig, NetHandle, SessionTiming, TokenSink};
use crate::scene::tests::{def_in, places, talk, Asked, Scripted};
use crate::scene::{Built, SceneBuilder};
use clap::Parser;
use fmv_adapter_api::{AreaInfo, GameAdapter};
use fmv_core::{AccountId, AvatarAppearance, EntitySnapshot, SessionId};
use fmv_fixture::rooms::{RoomsAdapter, HALL};
use fmv_protocol::{
    read_msg, write_msg, Auth, ClientMsg, NewCharacter, NpcSnapshot, SceneOutcome, SceneParticipant, SceneRefusal, SceneRole, SceneRunId, ServerMsg,
    PROTOCOL_VERSION, SCENE_NPC_ID_BASE,
};
use std::collections::VecDeque;

const SCENE_ID: &str = "ff7/mds7pb_1/tifa-bar";
const SPAWN: Pos = Pos { x: 100.0, y: 130.0, z: 0.0 };

/// What the fake server does.
#[derive(Clone)]
struct Script {
    /// Answer a preview request with this refusal instead of the field's snapshot.
    refuse: Option<SceneRefusal>,
    /// Start the run when the trigger is pressed (as the owner).
    start_on_press: bool,
    /// Start a run for the player as a watcher right after the handshake, and relay these steps, then end it.
    watched: bool,
    /// A move after `SceneEnded` is ignored too (the player is never released).
    hold_for_ever: bool,
}

impl Script {
    fn owner() -> Self {
        Script { refuse: None, start_on_press: true, watched: false, hold_for_ever: false }
    }
}

#[derive(Debug, Default)]
struct Seen {
    messages: Vec<ClientMsg>,
}

impl Seen {
    fn scene_messages(&self) -> Vec<ClientMsg> {
        self.messages.iter().filter(|m| !matches!(m, ClientMsg::MoveIntent { .. } | ClientMsg::Ping { .. })).cloned().collect()
    }

    fn moves(&self) -> usize {
        self.messages.iter().filter(|m| matches!(m, ClientMsg::MoveIntent { .. })).count()
    }
}

/// The builder of the invented scene, for a field called `HALL`.
struct Invented;

impl SceneBuilder for Invented {
    fn build(&mut self, _scene: &str, _story: &fmv_ff7::story::StoryState, _seed: u32, _resources: fmv_ff7::script::scene::SceneResources) -> Result<Built, String> {
        Ok(Built { def: def_in(HALL), source: Box::new(Scripted { initial: places(), beats: VecDeque::from(talk()), asked: Asked::default() }) })
    }

    fn reach(&mut self, _index: usize, _area: &str) -> Option<f32> {
        Some(100.0)
    }
}

fn trigger() -> NpcSnapshot {
    NpcSnapshot { id: SCENE_NPC_ID_BASE, name: "tifa".into(), model: "aagb".into(), idle_animation: None, pos: Pos::new(SPAWN.x + 30.0, SPAWN.y, 0.0), facing: 0.0 }
}

fn me(pos: Pos) -> EntitySnapshot {
    EntitySnapshot { id: EntityId(7), pos, facing: 0.0, anim: AnimState::Idle, avatar: AvatarAppearance::new("Me", [1, 2, 3], None) }
}

fn start_msg(you: SceneRole) -> ServerMsg {
    ServerMsg::SceneStart {
        run: SceneRunId(1),
        scene: SCENE_ID.into(),
        seed: 5,
        progress: vec![1, 72, 0],
        gil: 0,
        replay: None,
        owner: EntityId(if you == SceneRole::Owner { 7 } else { 8 }),
        // The owner stands on spot 0, the one who watches on spot 1 (this player is whichever `you` says).
        participants: if you == SceneRole::Owner {
            vec![SceneParticipant { entity: EntityId(7), spot: 0 }, SceneParticipant { entity: EntityId(8), spot: 1 }]
        } else {
            vec![SceneParticipant { entity: EntityId(8), spot: 0 }, SceneParticipant { entity: EntityId(7), spot: 1 }]
        },
        you,
    }
}

async fn serve(listener: tokio::net::TcpListener, info: AreaInfo, script: Script) -> Seen {
    let (stream, _) = listener.accept().await.expect("a client");
    let (mut rd, mut wr) = stream.into_split();
    let _hello: ClientMsg = read_msg(&mut rd).await.expect("Hello");
    let welcome = ServerMsg::Welcome {
        protocol_version: PROTOCOL_VERSION,
        server_build: "mini".into(),
        session: SessionId(1),
        entity: EntityId(7),
        account: AccountId(1),
        issued_token: None,
        character: fmv_protocol::CharacterCard::plain("Me", [1, 2, 3]),
    };
    write_msg(&mut wr, &welcome).await.unwrap();
    let manifest = ServerMsg::AreaManifest {
        area: info.id.clone(),
        display_name: info.display_name.clone(),
        fingerprint: info.fingerprint,
        provenance: info.provenance.summary(),
        fixture: true,
    };
    write_msg(&mut wr, &manifest).await.unwrap();
    let _join: ClientMsg = read_msg(&mut rd).await.expect("JoinArea");
    let snapshot = || ServerMsg::AreaSnapshot { area: info.id.clone(), tick: 1, entities: vec![me(SPAWN)], npcs: vec![trigger()] };
    write_msg(&mut wr, &snapshot()).await.unwrap();
    let mut seen = Seen::default();
    let mut held = false;
    if script.watched {
        write_msg(&mut wr, &start_msg(SceneRole::Watcher)).await.unwrap();
        for step in 0..4u32 {
            let answer = (step == 2).then_some(1);
            write_msg(&mut wr, &ServerMsg::SceneStep { run: SceneRunId(1), step, answer }).await.unwrap();
        }
        write_msg(&mut wr, &ServerMsg::SceneEnded { run: SceneRunId(1), outcome: SceneOutcome::Finished }).await.unwrap();
        held = script.hold_for_ever;
    }
    loop {
        match read_msg::<_, ClientMsg>(&mut rd).await {
            Ok(ClientMsg::Leave) | Err(_) => break,
            Ok(msg) => {
                seen.messages.push(msg.clone());
                match msg {
                    ClientMsg::Ping { nonce } => write_msg(&mut wr, &ServerMsg::Pong { nonce, tick: 0 }).await.unwrap(),
                    ClientMsg::SceneDebugPreview { .. } => match script.refuse {
                        Some(reason) => write_msg(&mut wr, &ServerMsg::SceneRefused { reason }).await.unwrap(),
                        None => write_msg(&mut wr, &snapshot()).await.unwrap(),
                    },
                    ClientMsg::Interact { npc: Some(id) } if id == SCENE_NPC_ID_BASE && script.start_on_press => {
                        held = true;
                        write_msg(&mut wr, &start_msg(SceneRole::Owner)).await.unwrap();
                    }
                    ClientMsg::SceneEnd { run, skipped } => {
                        let outcome = if skipped { SceneOutcome::Skipped } else { SceneOutcome::Finished };
                        write_msg(&mut wr, &ServerMsg::SceneEnded { run, outcome }).await.unwrap();
                        held = script.hold_for_ever;
                    }
                    ClientMsg::SceneLeave { run } => {
                        write_msg(&mut wr, &ServerMsg::SceneEnded { run, outcome: SceneOutcome::Aborted }).await.unwrap();
                        held = script.hold_for_ever;
                    }
                    // A player that is held has its moves ignored (no answer); a free one is echoed its own place.
                    ClientMsg::MoveIntent { pos, .. } if !held => {
                        write_msg(&mut wr, &ServerMsg::EntityUpdate { tick: 2, entities: vec![me(pos)] }).await.unwrap();
                    }
                    _ => {}
                }
            }
        }
    }
    seen
}

fn config(addr: std::net::SocketAddr) -> NetConfig {
    NetConfig {
        server: addr.to_string(),
        auth: Auth::Register { display_name: "Me".into() },
        character: NewCharacter::with_tint([1, 2, 3]),
        client_build: "test".into(),
        timing: SessionTiming::DEFAULT,
        token_sink: TokenSink::new(|_| Ok(())),
    }
}

/// What a run came to: the exit code, the lines the bot printed on stdout, what the fake server saw.
struct Run {
    code: u8,
    lines: Vec<String>,
    seen: Seen,
}

/// Run the headless client with `flags` against a fake server doing `script`; `builder` is what plays scenes (`None`: a
/// machine without game data).
async fn run_scene_client(flags: &[&str], script: Script, builder: bool) -> Run {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let info = RoomsAdapter::new().list_areas().into_iter().find(|i| i.id.key == HALL).expect("the hall");
    let server = tokio::spawn(serve(listener, info, script));
    let mut argv = vec!["fmv-client", "--name", "Me", "--headless", "--mute"];
    argv.extend_from_slice(flags);
    let args = crate::cli::Args::try_parse_from(argv).expect("arguments");
    let client = tokio::task::spawn_blocking(move || {
        let mut net = NetHandle::spawn(config(addr), Box::new(RoomsAdapter::new())).expect("net thread");
        let joined = net.wait_joined().expect("joined");
        let mut session = Session::new(net, joined).with_content(Arc::new(RoomsAdapter::new()));
        if builder {
            session.state.scene.set_builder(Box::new(Invented));
        }
        let code = run(&args, session, None, GameAudio::off());
        (code, super::take_scene_lines())
    });
    let (code, lines) = client.await.expect("the client thread");
    Run { code, lines, seen: server.await.expect("the server task") }
}

fn owner_lines() -> Vec<String> {
    [
        "scene started run=1 scene=ff7/mds7pb_1/tifa-bar role=Owner participants=2",
        "scene line index=0 opener=18 speaker=18 dialog=1 asks=false answer=none",
        "scene line index=1 opener=20 speaker=20 dialog=2 asks=false answer=none",
        "scene line index=2 opener=18 speaker=18 dialog=3 asks=true answer=1",
        "scene line index=3 opener=20 speaker=20 dialog=4 asks=false answer=none",
        "scene ended run=1 outcome=Finished lines=4",
        "scene control restored moved=true",
    ]
    .iter()
    .map(ToString::to_string)
    .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_owner_bot_previews_presses_plays_every_window_reports_it_and_proves_control_came_back() {
    let run = run_scene_client(&["--scene-preview", SCENE_ID, "--scene-wait-secs", "20"], Script::owner(), true).await;
    assert_eq!(run.code, 0, "{:?}", run.lines);
    let mut want = vec![format!("scene preview requested scene={SCENE_ID}")];
    want.extend(owner_lines());
    assert_eq!(run.lines, want);
    // What the server heard, in order: the preview, the press on the trigger, the four steps (the question's answer on the
    // third), the end, and exactly one move (the probe): the bot stood still the whole time.
    let step = |step, answer| ClientMsg::SceneStep { run: SceneRunId(1), step, answer };
    assert_eq!(
        run.seen.scene_messages(),
        [
            ClientMsg::SceneDebugPreview { scene: SCENE_ID.into() },
            ClientMsg::Interact { npc: Some(SCENE_NPC_ID_BASE) },
            step(0, None),
            step(1, None),
            step(2, Some(1)),
            step(3, None),
            ClientMsg::SceneEnd { run: SceneRunId(1), skipped: false },
        ]
    );
    assert_eq!(run.seen.moves(), 1, "one move: the control check's");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_owner_bot_answers_a_question_with_the_line_it_was_given() {
    let run = run_scene_client(&["--scene-preview", SCENE_ID, "--scene-answer", "2", "--scene-wait-secs", "20"], Script::owner(), true).await;
    assert_eq!(run.code, 0, "{:?}", run.lines);
    assert!(run.lines.contains(&"scene line index=2 opener=18 speaker=18 dialog=3 asks=true answer=2".to_string()), "{:?}", run.lines);
    assert!(run.seen.messages.contains(&ClientMsg::SceneStep { run: SceneRunId(1), step: 2, answer: Some(2) }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_watcher_bot_prints_the_same_lines_as_the_owner_and_sends_nothing_about_the_scene() {
    let script = Script { refuse: None, start_on_press: false, watched: true, hold_for_ever: false };
    let run = run_scene_client(&["--scene-watch", "--scene-wait-secs", "20"], script, true).await;
    assert_eq!(run.code, 0, "{:?}", run.lines);
    let mut want = owner_lines();
    want[0] = "scene started run=1 scene=ff7/mds7pb_1/tifa-bar role=Watcher participants=2".to_string();
    assert_eq!(run.lines, want, "the same lines the owner prints, from the owner's steps");
    assert_eq!(run.seen.scene_messages(), [], "a watcher says nothing about the scene (not even a press or a preview)");
    assert_eq!(run.seen.moves(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bot_that_the_server_never_releases_exits_nonzero_saying_control_did_not_come_back() {
    let script = Script { hold_for_ever: true, ..Script::owner() };
    let run = run_scene_client(&["--scene-preview", SCENE_ID, "--scene-wait-secs", "20"], script, true).await;
    assert_eq!(run.code, 1);
    assert_eq!(run.lines.last().map(String::as_str), Some("scene control restored moved=false"));
    assert_eq!(run.lines.iter().filter(|l| l.starts_with("scene line")).count(), 4, "it still played the scene");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_preview_is_printed_and_fails_the_run() {
    let script = Script { refuse: Some(SceneRefusal::Unavailable), ..Script::owner() };
    let run = run_scene_client(&["--scene-preview", SCENE_ID, "--scene-wait-secs", "20"], script, true).await;
    assert_eq!(run.code, 1);
    assert_eq!(run.lines, [format!("scene preview requested scene={SCENE_ID}"), "scene refused reason=Unavailable".to_string()]);
    assert_eq!(run.seen.scene_messages(), [ClientMsg::SceneDebugPreview { scene: SCENE_ID.into() }], "it pressed nothing");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_machine_that_cannot_build_the_scene_says_so_releases_the_player_and_fails_the_run() {
    let run = run_scene_client(&["--scene-preview", SCENE_ID, "--scene-wait-secs", "20"], Script::owner(), false).await;
    assert_eq!(run.code, 1);
    assert!(run.lines.contains(&"scene unavailable (cannot read this machine's field data)".to_string()), "{:?}", run.lines);
    assert!(run.lines.iter().all(|l| !l.starts_with("scene line")), "no line of a scene it could not build");
    // The player asks to be let go at once, and says nothing that could move the story: a scene this machine could not
    // show is LEFT (the server ends the run without applying anything), never reported as skipped.
    assert!(run.seen.messages.contains(&ClientMsg::SceneLeave { run: SceneRunId(1) }), "{:?}", run.seen.messages);
    assert!(!run.seen.messages.iter().any(|m| matches!(m, ClientMsg::SceneEnd { .. })), "{:?}", run.seen.messages);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bot_whose_scene_never_starts_gives_up_after_the_wait_and_fails_the_run() {
    let script = Script { start_on_press: false, ..Script::owner() };
    let started = Instant::now();
    let run = run_scene_client(&["--scene-preview", SCENE_ID, "--scene-wait-secs", "1.5"], script, true).await;
    assert_eq!(run.code, 1);
    assert!(started.elapsed() < Duration::from_secs(15), "it gave up by itself");
    assert!(run.lines.iter().all(|l| !l.starts_with("scene started")), "{:?}", run.lines);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_run_without_a_scene_flag_prints_no_scene_line_and_owes_the_server_nothing() {
    let script = Script::owner();
    let run = run_scene_client(&["--duration", "0.5"], script, true).await;
    assert_eq!(run.code, 0);
    assert!(run.lines.is_empty());
    assert_eq!(run.seen.scene_messages(), []);
}
