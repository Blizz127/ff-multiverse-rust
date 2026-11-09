//! The headless loop (`run`) against a MINI SERVER over a real socket, in the areas that need presses and links: the
//! fixture rooms with their interactions and the mock area of `crate::route::tests`. The mini server keeps the real
//! server's rules for what these tests are about, from the same pieces the adapter API gives it (`LinkArming`, `Link`,
//! `StoryPoint::is_pressed_at`): a walk-on link fires when a move touches its line while armed, an OK link on a press
//! where the player stands, the client is told with `MoveRejected { seq, corrected }`, every intent is answered with the
//! destination until one agrees with it (the fence) and a press while fenced does nothing. So the bots are judged on the
//! whole loop: when an intent goes out, when a press does, which `seq` a correction carries.

use super::*;
use crate::audio::GameAudio;
use crate::net::{NetConfig, NetHandle, SessionTiming, TokenSink};
use crate::route::tests::Floors;
use clap::Parser;
use fmv_adapter_api::{AdapterError, AreaInfo, AreaRuntime, GameAdapter, Link, LinkArming, StoryPoint, StoryTrigger};
use fmv_core::{AccountId, AvatarAppearance, EntitySnapshot, GameId, SessionId};
use fmv_fixture::rooms::{RoomsAdapter, HALL, STORY_ADVANCED, YARD, YARD_NPC_POS};
use fmv_protocol::{read_msg, write_msg, Auth, ClientMsg, NewCharacter, NpcSnapshot, ServerMsg, StoryAdvance, PROTOCOL_VERSION};

/// What the mini server saw and did.
#[derive(Debug, Default)]
struct Seen {
    /// The intents and presses it received, in order.
    messages: Vec<ClientMsg>,
    /// The links it fired, in order.
    fired: Vec<usize>,
    /// The story the player has now.
    progress: Vec<u8>,
    /// Where it holds the player at the end.
    pos: Pos,
    /// The presses it ignored because the player was fenced.
    ignored_while_fenced: u32,
}

struct Mini {
    area: Box<dyn AreaRuntime>,
    links: Vec<Link>,
    points: Vec<StoryPoint>,
    npcs: Vec<NpcSnapshot>,
    pos: Pos,
    arming: LinkArming,
    fence: Option<Pos>,
    last_seq: u32,
    seen: Seen,
}

impl Mini {
    fn new(area: Box<dyn AreaRuntime>, npcs: Vec<NpcSnapshot>, spawn: Pos) -> Self {
        let links = area.links();
        let points = area.story_points();
        let arming = LinkArming::placed_at(&links, spawn);
        Self { area, links, points, npcs, pos: spawn, arming, fence: None, last_seq: 0, seen: Seen { pos: spawn, ..Seen::default() } }
    }

    /// A link put the player at its destination: it is fenced there until an intent agrees, and its arming starts over.
    fn relocate(&mut self, index: usize) -> Pos {
        let link = &self.links[index];
        let dest = self.area.place_arrival(link.to, link.to_hint);
        self.pos = dest;
        self.arming = LinkArming::placed_at(&self.links, dest);
        self.fence = Some(dest);
        self.seen.fired.push(index);
        dest
    }

    /// What the server answers to one message of the client.
    fn on_message(&mut self, msg: ClientMsg) -> Vec<ServerMsg> {
        match &msg {
            ClientMsg::MoveIntent { seq, pos: to, .. } => {
                let seq = *seq;
                self.seen.messages.push(msg.clone());
                self.last_seq = seq;
                if let Some(dest) = self.fence {
                    if to.dist_xy(dest) <= 60.0 && (to.z - dest.z).abs() <= 80.0 {
                        self.fence = None;
                    } else {
                        return vec![ServerMsg::MoveRejected { seq, corrected: dest }];
                    }
                }
                match self.area.validate_move(self.pos, *to) {
                    fmv_adapter_api::MoveResult::Accepted(accepted) => {
                        let from = self.pos;
                        self.pos = accepted;
                        self.seen.pos = accepted;
                        match self.arming.step(&self.links, from, accepted) {
                            Some(index) => {
                                let corrected = self.relocate(index);
                                self.seen.pos = corrected;
                                vec![ServerMsg::MoveRejected { seq, corrected }]
                            }
                            None => Vec::new(),
                        }
                    }
                    other => vec![ServerMsg::MoveRejected { seq, corrected: other.pos() }],
                }
            }
            ClientMsg::Interact { npc } => {
                self.seen.messages.push(msg.clone());
                if self.fence.is_some() {
                    self.seen.ignored_while_fenced += 1;
                    return Vec::new();
                }
                let addressed = npc.and_then(|id| self.npcs.iter().find(|n| n.id == id)).map(|n| n.pos);
                let talking = addressed.is_some();
                let point = self.points.iter().find(|p| {
                    matches!(p.trigger, StoryTrigger::Talk { .. }) == talking
                        && p.is_pressed_at(self.pos, addressed)
                        && self.area.story_advance(&self.seen.progress, p.id).is_some()
                });
                if let Some(point) = point {
                    let step = self.area.story_advance(&self.seen.progress, point.id).expect("checked");
                    self.seen.progress = step.progress.clone();
                    return vec![ServerMsg::StoryUpdate { progress: step.progress, advance: Some(StoryAdvance { point: point.id, summary: step.summary }) }];
                }
                if !talking {
                    if let Some(index) = self.links.iter().position(|l| l.needs_ok && l.is_touched_at(self.pos)) {
                        let corrected = self.relocate(index);
                        self.seen.pos = corrected;
                        return vec![ServerMsg::MoveRejected { seq: self.last_seq, corrected }];
                    }
                }
                Vec::new()
            }
            ClientMsg::Ping { nonce } => vec![ServerMsg::Pong { nonce: *nonce, tick: 0 }],
            _ => Vec::new(),
        }
    }
}

/// Serve ONE connection as the mini server: the handshake into `info`'s area, then `mini` until the client leaves.
async fn serve(listener: tokio::net::TcpListener, info: AreaInfo, mut mini: Mini) -> Seen {
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
    let me = EntitySnapshot { id: EntityId(7), pos: mini.pos, facing: 0.0, anim: AnimState::Idle, avatar: AvatarAppearance::new("Me", [1, 2, 3], None) };
    let snapshot = ServerMsg::AreaSnapshot { area: info.id.clone(), tick: 1, entities: vec![me], npcs: mini.npcs.clone() };
    write_msg(&mut wr, &snapshot).await.unwrap();
    loop {
        match read_msg::<_, ClientMsg>(&mut rd).await {
            Ok(ClientMsg::Leave) | Err(_) => break,
            Ok(msg) => {
                for answer in mini.on_message(msg) {
                    write_msg(&mut wr, &answer).await.unwrap();
                }
            }
        }
    }
    mini.seen
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

/// The local content of a client that holds the mock area of `crate::route::tests` as its one area.
struct FloorsContent(AreaInfo);

impl GameAdapter for FloorsContent {
    fn game(&self) -> GameId {
        GameId::Fixture
    }
    fn list_areas(&self) -> Vec<AreaInfo> {
        vec![self.0.clone()]
    }
    fn load_area(&self, key: &str) -> Result<Box<dyn AreaRuntime>, AdapterError> {
        if key == self.0.id.key {
            Ok(Box::new(Floors::new(true).with_story()))
        } else {
            Err(AdapterError::AreaNotFound(key.to_string()))
        }
    }
}

/// Run the headless client with `flags` for `secs` seconds against `mini`, which serves the area `info`; the content
/// of the client is what `content` makes (twice: the handshake takes one, the session keeps the other).
async fn run_client(flags: &[&str], secs: f64, content: impl Fn() -> Box<dyn GameAdapter> + Send + 'static, info: AreaInfo, mini: Mini) -> (u8, Seen) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(serve(listener, info, mini));
    let mut argv = vec!["fmv-client", "--name", "Me", "--headless", "--mute"];
    argv.extend_from_slice(flags);
    let mut args = crate::cli::Args::try_parse_from(argv).expect("arguments");
    args.duration = Duration::from_secs_f64(secs);
    let client = tokio::task::spawn_blocking(move || {
        let mut net = NetHandle::spawn(config(addr), content()).expect("net thread");
        let joined = net.wait_joined().expect("joined");
        let session = Session::new(net, joined).with_content(Arc::from(content()));
        run(&args, session, None, GameAudio::off())
    });
    let code = client.await.expect("the client thread");
    (code, server.await.expect("the server task"))
}

fn info_of(key: &str) -> AreaInfo {
    RoomsAdapter::new().list_areas().into_iter().find(|i| i.id.key == key).expect("a listed room")
}

fn presses(seen: &Seen) -> Vec<Option<u32>> {
    seen.messages
        .iter()
        .filter_map(|m| match m {
            ClientMsg::Interact { npc } => Some(*npc),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_story_bot_walks_into_the_press_region_and_presses_ok_once_the_server_knows_where_it_stands() {
    let hall = RoomsAdapter::with_interactions().load_area(HALL).unwrap();
    let spawn = Pos::new(100.0, 130.0, 0.0);
    let mini = Mini::new(RoomsAdapter::with_interactions().load_area(HALL).unwrap(), Vec::new(), spawn);
    let (code, seen) = run_client(&["--story-advance"], 3.0, || Box::new(RoomsAdapter::with_interactions()), info_of(HALL), mini).await;
    assert_eq!(code, 0);
    // One press, from where the press region is: the server answered it with the step, which ends the bot's business.
    assert_eq!(presses(&seen), [None], "{:?}", seen.messages);
    assert_eq!(seen.progress, STORY_ADVANCED, "the server paid the step for the press");
    assert_eq!(seen.ignored_while_fenced, 0);
    // The press came after an intent that put the player in the region: the last intent before it is in the region.
    let press = hall.story_points().into_iter().find(|p| p.id == 1).unwrap();
    let at = seen.messages.iter().position(|m| matches!(m, ClientMsg::Interact { .. })).unwrap();
    let ClientMsg::MoveIntent { pos, .. } = seen.messages[..at].iter().rev().find(|m| matches!(m, ClientMsg::MoveIntent { .. })).unwrap() else { unreachable!() };
    assert!(press.is_pressed_at(*pos, None), "the last intent before the press stands in the region: {pos:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_story_bot_walks_up_to_the_guard_and_presses_ok_addressing_it() {
    let guard = NpcSnapshot {
        id: 4,
        name: "guard".into(),
        model: "guardm".into(),
        idle_animation: None,
        pos: Pos::new(YARD_NPC_POS.0, YARD_NPC_POS.1, 0.0),
        facing: 0.0,
    };
    let yard_spawn = Pos::new(-200.0, 0.0, 0.0);
    let mini = Mini::new(RoomsAdapter::with_interactions().load_area(YARD).unwrap(), vec![guard], yard_spawn);
    let (code, seen) = run_client(&["--story-advance"], 4.0, || Box::new(RoomsAdapter::with_interactions()), info_of(YARD), mini).await;
    assert_eq!(code, 0);
    assert_eq!(presses(&seen), [Some(4)], "{:?}", seen.messages);
    assert_eq!(seen.progress, STORY_ADVANCED, "the talk moved the story");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_travel_and_story_bot_goes_by_a_jump_and_a_ladder_to_the_panel_and_on_to_the_exit() {
    let info = info_of(HALL);
    let spawn = Pos::new(-200.0, 0.0, 0.0);
    let mini = Mini::new(Box::new(Floors::new(true).with_story()), Vec::new(), spawn);
    let content_info = info.clone();
    let (code, seen) = run_client(&["--story-advance", "--travel", "next"], 9.0, move || Box::new(FloorsContent(content_info.clone())), info, mini).await;
    assert_eq!(code, 0);
    // the jump (link 0) fired by walking, the ladder (link 1) by a press at its foot, the panel by a press in its region
    assert_eq!(seen.fired, [0, 1], "{:?}", seen.messages);
    assert!(presses(&seen).starts_with(&[None, None]), "the ladder's press, then the panel's: {:?}", seen.messages);
    assert_eq!(seen.progress, vec![1], "the panel moved the story");
    assert_eq!(seen.ignored_while_fenced, 0, "no press was sent before the server had been told where the bot stood");
    // and on to the exit's line (x = 300) in the loft
    assert!(seen.pos.x > 250.0 && (seen.pos.z - 500.0).abs() < 1.0, "it walked on in the loft: {:?}", seen.pos);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_story_bot_does_not_press_the_toggle_region_after_the_press_region_has_paid() {
    // A bot that stands in the press region, with the toggle region (OK there takes an advanced story back to a new
    // account's) a few steps away: after the press region's step it must leave the toggle alone, so the story ends
    // where the press left it. (The server applies a press as the point says, forward or not.)
    let spawn = Pos::new(200.0, 130.0, 0.0);
    let mini = Mini::new(RoomsAdapter::with_interactions().load_area(HALL).unwrap(), Vec::new(), spawn);
    let (code, seen) = run_client(&["--story-advance"], 5.0, || Box::new(RoomsAdapter::with_interactions()), info_of(HALL), mini).await;
    assert_eq!(code, 0);
    assert_eq!(presses(&seen), [None], "one press, the press region's: {:?}", seen.messages);
    assert_eq!(seen.progress, STORY_ADVANCED, "the story stayed where the press region put it");
    assert_eq!(seen.fired, Vec::<usize>::new());
}
