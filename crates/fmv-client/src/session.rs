//! A joined session: the local area, the replicated state and the network handle, shared by the
//! headless bot loop and the window loop.

use crate::battle::BattleCmd;
use crate::net::{check_local_content, clean, AreaAnswer, Joined, LocalContent, NetCommand, NetEvent, NetHandle, PartyCommand, UNKNOWN_CHARACTER_NAME};
use crate::state::ClientState;
use fmv_adapter_api::{AreaRuntime, GameAdapter};
use fmv_core::{AreaId, AvatarAppearance, EntityId, EntitySnapshot, Fingerprint, Pos, ProvenanceSummary};
use fmv_protocol::{CharacterCard, ServerMsg};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// What we know about the joined area (for titles, banners and logs).
#[derive(Debug, Clone)]
pub struct SessionInfo {
    /// Our display name as the SERVER shows it (the stored profile, not the command line).
    pub name: String,
    /// Our tint as the server shows it.
    pub tint: [u8; 3],
    pub area_id: AreaId,
    /// True for synthetic FIXTURE content, which must be labelled everywhere it is surfaced.
    pub fixture: bool,
    /// What the server holds of our own character (protocol v8; cleaned and bounded, see
    /// `crate::net::keep_card`): its name, colour, body choice, how the game draws it (the binding) and the
    /// combat slot it fights with. What the window and the headless bot show of who we are.
    pub card: CharacterCard,
}

/// Shown for our own entity if the first snapshot (against the protocol) does not contain it.
const UNKNOWN_NAME: &str = "?";
const UNKNOWN_TINT: [u8; 3] = [255, 255, 255];

/// Our display name and tint, as the server describes our own entity in the first snapshot. The
/// server is the authority: after registration it takes both from the stored avatar profile, so
/// the command line says nothing about them. Server text is cleaned like any other.
pub fn own_identity(entities: &[EntitySnapshot], entity: EntityId) -> (String, [u8; 3]) {
    match entities.iter().find(|e| e.id == entity) {
        Some(e) => (
            AvatarAppearance::sanitize_name(&e.avatar.display_name).unwrap_or_else(|| UNKNOWN_NAME.to_string()),
            e.avatar.tint,
        ),
        None => (UNKNOWN_NAME.to_string(), UNKNOWN_TINT),
    }
}

/// What the server's `AreaChange` offered and this session accepted (see [`Session::take_area_change`]).
#[derive(Debug, Clone, PartialEq)]
pub struct AreaChanged {
    pub area: AreaId,
    /// Where the server put us in it.
    pub arrival: Pos,
    /// A change to another area (not the way back after a transfer this client refused).
    pub forward: bool,
}

/// The server's `AreaChange`, as the session receives it.
struct AreaOffer {
    area: AreaId,
    display_name: String,
    fingerprint: Fingerprint,
    provenance: ProvenanceSummary,
    fixture: bool,
    arrival: Pos,
    arrival_facing: f32,
}

pub struct Session {
    pub net: NetHandle,
    /// The area decoded from this machine's own content (used for local movement validation).
    /// Replaced when the server moves us to another area.
    pub area: Box<dyn AreaRuntime>,
    pub state: ClientState,
    pub info: SessionInfo,
    origin: Instant,
    /// The session already has to end because of a message from the handshake backlog; the first
    /// `pump` reports it.
    pending_end: Option<String>,
    /// This machine's own game content, to check an `AreaChange` against and load the new area
    /// from (the same content the handshake used). Without it the session cannot follow the server
    /// to another area: it refuses (`CannotJoin`) and the server sends it back.
    content: Option<Arc<dyn GameAdapter>>,
    /// The last area change not yet picked up by [`Session::take_area_change`].
    area_changed: Option<AreaChanged>,
}

impl Session {
    /// Build the session from a finished handshake and apply the messages that arrived with it.
    pub fn new(net: NetHandle, joined: Joined) -> Self {
        let Joined {
            entity,
            account,
            character,
            area_id,
            fixture,
            area,
            entities,
            backlog,
            ..
        } = joined;
        let spawn = entities
            .iter()
            .find(|e| e.id == entity && e.pos.is_finite())
            .map(|e| e.pos)
            .or_else(|| area.spawn_points().first().copied())
            .unwrap_or(Pos::new(0.0, 0.0, 0.0));
        let origin = Instant::now();
        let mut state = ClientState::new(entity, account, spawn, area_id.clone());
        // Who we are, BEFORE the backlog is applied: the progress the server sent on joining is noted (and
        // shown) with the player's name, which the mirror cannot know otherwise.
        let (name, tint) = own_identity(&entities, entity);
        let player = if character.name == UNKNOWN_CHARACTER_NAME { name.as_str() } else { character.name.as_str() };
        state.progress.set_player(player);
        let mut pending_end = None;
        for msg in backlog {
            // The handshake already refuses a Reject, a foreign snapshot and a second manifest, so
            // nothing here should end the session; if something does, say so on the first pump.
            if let Some(reason) = state.apply(0.0, msg) {
                pending_end.get_or_insert(reason);
            }
        }
        let info = SessionInfo {
            name,
            tint,
            area_id,
            fixture,
            card: character,
        };
        Self {
            net,
            area,
            state,
            info,
            origin,
            pending_end,
            content: None,
            area_changed: None,
        }
    }

    /// Give the session this machine's own game content, so it can follow the server to other
    /// areas (see the `content` field).
    pub fn with_content(mut self, content: Arc<dyn GameAdapter>) -> Self {
        self.content = Some(content);
        self
    }

    /// This machine's own game content, when the session holds it (see [`Session::with_content`]): what a bot asks
    /// the game's own questions of (is this story step forward of the story the player has?).
    pub fn content(&self) -> Option<Arc<dyn GameAdapter>> {
        self.content.clone()
    }

    /// The area change that happened since the last call, if any: the window rebuilds its view and
    /// the headless bot its walking pattern for the new area. Several changes between two calls
    /// are reported as the last one.
    pub fn take_area_change(&mut self) -> Option<AreaChanged> {
        self.area_changed.take()
    }

    /// Seconds since the session started (monotonic).
    pub fn now(&self) -> f64 {
        self.origin.elapsed().as_secs_f64()
    }

    /// Apply everything the network thread has queued. Returns `Some(reason)` when the session
    /// ended abnormally (connection lost, or the server ended it).
    pub fn pump(&mut self) -> Option<String> {
        if let Some(reason) = self.pending_end.take() {
            return Some(reason);
        }
        while let Some(ev) = self.net.try_next() {
            let now = self.now();
            match ev {
                // The server moves us to another area. Handled here and in order with everything
                // else: the answer is sent before the snapshot of the new area can be read.
                NetEvent::Server(ServerMsg::AreaChange {
                    area,
                    display_name,
                    fingerprint,
                    provenance,
                    fixture,
                    arrival,
                    arrival_facing,
                }) => self.on_area_change(AreaOffer { area, display_name, fingerprint, provenance, fixture, arrival, arrival_facing }),
                NetEvent::Server(msg) => {
                    if let Some(reason) = self.state.apply(now, msg) {
                        return Some(reason);
                    }
                }
                NetEvent::Rtt(rtt) => self.state.set_rtt(rtt),
                NetEvent::Closed(reason) => return Some(format!("connection to the server lost: {reason}")),
                // Only produced before the session starts.
                NetEvent::Joined(_) | NetEvent::Failed(_) => {}
            }
        }
        None
    }

    /// The server offers another area (`AreaChange`): check it against this machine's own content,
    /// exactly like the handshake does for the first area. If we hold it, answer `JoinArea` (with our
    /// OWN provenance), load it as the area we walk in, and tell the state; if not, answer
    /// `CannotJoin` and stay where we are (the server sends us back with another `AreaChange`).
    fn on_area_change(&mut self, offer: AreaOffer) {
        let AreaOffer { area, display_name, fingerprint, provenance, fixture, arrival, arrival_facing } = offer;
        let name = clean(&display_name);
        let checked = match &self.content {
            Some(content) => check_local_content(content.as_ref(), &area, &fingerprint, &provenance),
            None => Err(crate::net::ContentMismatch {
                wire: "this client has no local content to load the area from".to_string(),
                local: "this session has no local content to load areas from".to_string(),
            }),
        };
        match checked {
            Ok(LocalContent { runtime, provenance: own }) => {
                self.net.send(NetCommand::Area(AreaAnswer::Join { area: area.clone(), fingerprint, provenance: own }));
                // Where the server says we stand, if it says something usable; else on our own
                // area's first spawn point (the snapshot that follows corrects it anyway).
                let arrival = if arrival.is_finite() {
                    arrival
                } else {
                    runtime.spawn_points().first().copied().unwrap_or_default()
                };
                let forward = area != self.info.area_id;
                eprintln!(
                    "travel: {} `{name}` ({}), arriving at ({:.0}, {:.0}); {} peer(s) were seen in `{}`, the area left",
                    if forward { "entering" } else { "back in" },
                    clean(&area.to_string()),
                    arrival.x,
                    arrival.y,
                    self.state.peers_seen(),
                    self.info.area_id
                );
                // FIXTURE is judged from OUR content as well as the server's flag, like at the
                // handshake: synthetic content can never be presented as real by a wrong flag.
                self.info.fixture = fixture || runtime.info().fixture || area.game.is_fixture();
                self.info.area_id = area.clone();
                self.area = runtime;
                self.state.change_area(area.clone(), arrival, arrival_facing, forward);
                self.area_changed = Some(AreaChanged { area, arrival, forward });
            }
            Err(mismatch) => {
                eprintln!("travel: cannot hold area `{}`: {} (staying; the server sends us back)", clean(&area.to_string()), mismatch.local);
                self.net.send(NetCommand::Area(AreaAnswer::Cannot { area, reason: mismatch.wire }));
            }
        }
    }

    /// Send the next `MoveIntent` if one is due (rate limited, only on change).
    pub fn flush_intent(&mut self) {
        let now = self.now();
        if let Some(intent) = self.state.own.next_intent(now) {
            self.net.send(NetCommand::Intent(intent));
        }
    }

    /// The player pressed OK: tell the server (`ClientMsg::Interact`), with the character addressed (`NpcView::id`) if
    /// there is one. The server works out what the press does from where it holds the player, so the intent that told it
    /// where we stand must have gone out first: the network thread sends commands in the order they were given, and
    /// a bot that needs the press to be judged where it stands waits until nothing is unsent
    /// ([`crate::state::OwnPlayer::has_unsent`]). Whatever the press does comes back as a `StoryUpdate` or a `MoveRejected`
    /// (a ladder), which the mirrors apply as always.
    pub fn send_interact(&self, npc: Option<u32>) {
        self.net.send(NetCommand::Interact { npc });
    }

    /// Send a party request (the server decides and answers with `PartyEvent` / `PartyRejected`).
    pub fn send_party(&self, cmd: PartyCommand) {
        self.net.send(NetCommand::Party(cmd));
    }

    /// Ask the server for the full party state if the mirror noticed it is out of step. Cheap to
    /// call every tick: the mirror rate limits the request.
    pub fn flush_party_sync(&mut self) {
        let now = self.now();
        if self.state.party.sync_due(now) {
            self.send_party(PartyCommand::Sync);
        }
    }

    /// Print what happened to the party since the last call, one `party:` line each, on stderr
    /// (never stdout: the headless bot's stdout carries exactly one line).
    pub fn log_party_notes(&mut self) {
        for line in self.state.party.drain_notes() {
            eprintln!("party: {line}");
        }
    }

    /// Send a battle request or command (the server decides and answers with battle messages or
    /// `BattleRejected`). A command is remembered as in flight, so it is not sent twice.
    pub fn send_battle(&mut self, cmd: BattleCmd) {
        let now = self.now();
        self.state.battle.note_sent(&cmd, now);
        self.net.send(NetCommand::Battle(cmd));
    }

    /// Print what happened in battle since the last call, one `battle:` line each, on stderr
    /// (never stdout: the headless bot's stdout carries exactly one line).
    pub fn log_battle_notes(&mut self) {
        for line in self.state.battle.drain_notes() {
            eprintln!("battle: {line}");
        }
    }

    /// Print what the server said about our character since the last call, one `progress:` line each,
    /// on stderr (never stdout: the headless bot's stdout carries exactly one line).
    pub fn log_progress_notes(&mut self) {
        for line in self.state.progress.drain_notes() {
            eprintln!("progress: {line}");
        }
    }

    /// Print what the server said about our story since the last call, one `story:` line each, on
    /// stderr (never stdout: the headless bot's stdout carries exactly one line).
    pub fn log_story_notes(&mut self) {
        for line in self.state.story.drain_notes() {
            eprintln!("story: {line}");
        }
    }

    /// Send what the scene client owes the server (the owner's steps and end, a watcher's leave, a preview request), in order.
    pub fn flush_scene(&mut self) {
        for cmd in self.state.scene.take_commands() {
            self.net.send(NetCommand::Scene(cmd));
        }
    }

    /// Print what the scene client has to say about itself since the last call, one `scene:` line each, on stderr (ids and
    /// counts only; the headless bot's scene lines go to stdout, see `crate::scene::bot`).
    pub fn log_scene_notes(&mut self) {
        for line in self.state.scene.take_notes() {
            eprintln!("{line}");
        }
    }

    /// Print what the server said about the countdown of our story since the last call, one `countdown:` line each, on
    /// stderr (see `crate::ui::countdown`).
    pub fn log_countdown_notes(&mut self) {
        for line in self.state.countdown.drain_notes() {
            eprintln!("countdown: {line}");
        }
    }

    /// Say goodbye and wait (bounded) for the network thread to flush `Leave`.
    pub fn leave(&mut self, wait: Duration) {
        self.net.leave_and_join(wait);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_core::AnimState;

    fn entity(id: u64, name: &str, tint: [u8; 3]) -> EntitySnapshot {
        EntitySnapshot {
            id: EntityId(id),
            pos: Pos::default(),
            facing: 0.0,
            anim: AnimState::Idle,
            avatar: AvatarAppearance::new(name, tint, None),
        }
    }

    #[test]
    fn our_name_and_tint_come_from_our_own_entity_in_the_snapshot() {
        let entities = [entity(8, "Peer", [9, 9, 9]), entity(7, "Cloud", [1, 2, 3])];
        assert_eq!(own_identity(&entities, EntityId(7)), ("Cloud".to_string(), [1, 2, 3]));
        assert_eq!(own_identity(&entities, EntityId(8)), ("Peer".to_string(), [9, 9, 9]));
    }

    // ---- following the server to another area (against a fake server on a real socket) ----

    use crate::net::{NetConfig, SessionTiming, TokenSink};
    use fmv_adapter_api::{AdapterError, AreaInfo};
    use fmv_core::{AccountId, GameId, SessionId};
    use fmv_fixture::rooms::{RoomsAdapter, HALL, YARD, YARD_ARRIVAL};
    use fmv_protocol::{read_msg, write_msg, Auth, ClientMsg, NewCharacter, NpcSnapshot, ServerMsg, PROTOCOL_VERSION};

    /// Only the hall of the rooms: a machine that does not hold the yard.
    struct OnlyHall(RoomsAdapter);

    impl GameAdapter for OnlyHall {
        fn game(&self) -> GameId {
            GameId::Fixture
        }
        fn list_areas(&self) -> Vec<AreaInfo> {
            self.0.list_areas().into_iter().filter(|i| i.id.key == HALL).collect()
        }
        fn load_area(&self, key: &str) -> Result<Box<dyn AreaRuntime>, AdapterError> {
            if key == HALL {
                self.0.load_area(key)
            } else {
                Err(AdapterError::AreaNotFound(key.to_string()))
            }
        }
    }

    fn info_of(key: &str) -> AreaInfo {
        RoomsAdapter::new().list_areas().into_iter().find(|i| i.id.key == key).expect("a listed room")
    }

    fn me(pos: Pos) -> EntitySnapshot {
        EntitySnapshot { id: EntityId(7), pos, facing: 0.0, anim: fmv_core::AnimState::Idle, avatar: AvatarAppearance::new("Me", [1, 2, 3], None) }
    }

    fn change_for(key: &str, arrival: Pos) -> ServerMsg {
        let info = info_of(key);
        ServerMsg::AreaChange {
            area: info.id,
            display_name: info.display_name,
            fingerprint: info.fingerprint,
            provenance: info.provenance.summary(),
            fixture: true,
            arrival,
            arrival_facing: 1.5,
        }
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

    /// The server side of ONE connection: the handshake into the hall, then the `AreaChange`
    /// offers in `offers`, one after the other (each waits for its answer; a `JoinArea` is
    /// followed by the snapshot of that area, with one NPC, like the real server does), and the
    /// answers it got. Ends when the client says `Leave`.
    async fn serve(listener: tokio::net::TcpListener, offers: Vec<ServerMsg>) -> Vec<ClientMsg> {
        let (stream, _) = listener.accept().await.expect("a client");
        let (mut rd, mut wr) = stream.into_split();
        let mut answers = Vec::new();
        let hall = info_of(HALL);
        let spawn = Pos::new(-100.0, 0.0, 0.0);
        let _hello: ClientMsg = read_msg(&mut rd).await.expect("Hello");
        let welcome = ServerMsg::Welcome {
            protocol_version: PROTOCOL_VERSION,
            server_build: "fake".into(),
            session: SessionId(1),
            entity: EntityId(7),
            account: AccountId(1),
            issued_token: None,
            character: CharacterCard::plain("Me", [1, 2, 3]),
        };
        write_msg(&mut wr, &welcome).await.unwrap();
        let manifest = ServerMsg::AreaManifest {
            area: hall.id.clone(),
            display_name: hall.display_name.clone(),
            fingerprint: hall.fingerprint,
            provenance: hall.provenance.summary(),
            fixture: true,
        };
        write_msg(&mut wr, &manifest).await.unwrap();
        let join: ClientMsg = read_msg(&mut rd).await.expect("JoinArea");
        answers.push(join);
        write_msg(&mut wr, &ServerMsg::AreaSnapshot { area: hall.id.clone(), tick: 1, entities: vec![me(spawn)], npcs: vec![] })
            .await
            .unwrap();
        for offer in offers {
            write_msg(&mut wr, &offer).await.unwrap();
            let ServerMsg::AreaChange { area, arrival, .. } = &offer else { panic!("offers are AreaChanges") };
            loop {
                match read_msg::<_, ClientMsg>(&mut rd).await.expect("an answer") {
                    ClientMsg::Ping { .. } | ClientMsg::MoveIntent { .. } => {}
                    answer @ ClientMsg::JoinArea { .. } => {
                        answers.push(answer);
                        let npc = NpcSnapshot {
                            id: 0,
                            name: "guard".into(),
                            model: "guardm".into(),
                            idle_animation: None,
                            pos: Pos::new(200.0, 100.0, 0.0),
                            facing: 0.5,
                        };
                        let snapshot = ServerMsg::AreaSnapshot { area: area.clone(), tick: 2, entities: vec![me(*arrival)], npcs: vec![npc] };
                        write_msg(&mut wr, &snapshot).await.unwrap();
                        break;
                    }
                    answer @ ClientMsg::CannotJoin { .. } => {
                        answers.push(answer);
                        break;
                    }
                    other => panic!("unexpected {other:?}"),
                }
            }
        }
        loop {
            match read_msg::<_, ClientMsg>(&mut rd).await {
                Ok(ClientMsg::Leave) | Err(_) => break,
                Ok(_) => {}
            }
        }
        answers
    }

    /// The server side of ONE connection that only listens: the handshake into the hall, then every `MoveIntent` and
    /// `Interact` the client sends, in the order they arrive, until it says `Leave`.
    async fn record(listener: tokio::net::TcpListener) -> Vec<ClientMsg> {
        let (stream, _) = listener.accept().await.expect("a client");
        let (mut rd, mut wr) = stream.into_split();
        let hall = info_of(HALL);
        let _hello: ClientMsg = read_msg(&mut rd).await.expect("Hello");
        let welcome = ServerMsg::Welcome {
            protocol_version: PROTOCOL_VERSION,
            server_build: "fake".into(),
            session: SessionId(1),
            entity: EntityId(7),
            account: AccountId(1),
            issued_token: None,
            character: CharacterCard::plain("Me", [1, 2, 3]),
        };
        write_msg(&mut wr, &welcome).await.unwrap();
        let manifest = ServerMsg::AreaManifest {
            area: hall.id.clone(),
            display_name: hall.display_name.clone(),
            fingerprint: hall.fingerprint,
            provenance: hall.provenance.summary(),
            fixture: true,
        };
        write_msg(&mut wr, &manifest).await.unwrap();
        let _join: ClientMsg = read_msg(&mut rd).await.expect("JoinArea");
        write_msg(&mut wr, &ServerMsg::AreaSnapshot { area: hall.id.clone(), tick: 1, entities: vec![me(Pos::new(-100.0, 0.0, 0.0))], npcs: vec![] })
            .await
            .unwrap();
        let mut got = Vec::new();
        loop {
            match read_msg::<_, ClientMsg>(&mut rd).await {
                Ok(msg @ (ClientMsg::MoveIntent { .. } | ClientMsg::Interact { .. })) => got.push(msg),
                Ok(ClientMsg::Leave) | Err(_) => break,
                Ok(_) => {}
            }
        }
        got
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn a_press_goes_out_after_the_intent_that_told_the_server_where_we_stand() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(record(listener));
        let client = tokio::task::spawn_blocking(move || {
            let mut net = NetHandle::spawn(config(addr), Box::new(RoomsAdapter::new())).expect("net thread");
            let joined = net.wait_joined().expect("joined");
            let mut session = Session::new(net, joined);
            // A step the server does not know of yet: a press now would be judged where it still holds us.
            session.state.own.set_motion(Pos::new(-90.0, 0.0, 0.0), 0.0, AnimState::Walk);
            assert!(session.state.own.has_unsent());
            session.flush_intent();
            assert!(!session.state.own.has_unsent(), "the intent went out");
            session.send_interact(Some(4));
            session.send_interact(None);
            session.leave(Duration::from_secs(2));
        });
        client.await.expect("the client thread");
        let got = server.await.expect("the server task");
        match &got[..] {
            [ClientMsg::MoveIntent { seq: 1, pos, .. }, ClientMsg::Interact { npc: Some(4) }, ClientMsg::Interact { npc: None }] => assert_eq!((pos.x, pos.y), (-90.0, 0.0)),
            other => panic!("the intent first, then the two presses, in that order: {other:?}"),
        }
    }

    /// Pump `session` until `done` says so (or ten seconds pass).
    fn pump_until(session: &mut Session, what: &str, mut done: impl FnMut(&mut Session) -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !done(session) {
            if let Some(reason) = session.pump() {
                panic!("the session ended while waiting for {what}: {reason}");
            }
            assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn an_area_change_is_answered_from_local_content_and_the_session_moves_to_the_new_area() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let arrival = Pos::new(YARD_ARRIVAL.0, YARD_ARRIVAL.1, 0.0);
        let server = tokio::spawn(serve(listener, vec![change_for(YARD, arrival)]));
        let client = tokio::task::spawn_blocking(move || {
            let mut net = NetHandle::spawn(config(addr), Box::new(RoomsAdapter::new())).expect("net thread");
            let joined = net.wait_joined().expect("joined");
            let mut session = Session::new(net, joined).with_content(Arc::new(RoomsAdapter::new()));
            assert_eq!((session.info.area_id.key.as_str(), session.state.area_changes()), (HALL, 0));
            let hall_speed = session.area.max_speed();
            // The offer arrives, is answered, and the session is in the yard...
            pump_until(&mut session, "the area change", |s| s.info.area_id.key == YARD);
            let changed = session.take_area_change().expect("the change is reported once");
            assert!(session.take_area_change().is_none());
            assert_eq!((changed.area.key.as_str(), changed.forward, changed.arrival), (YARD, true, arrival));
            assert_eq!(session.state.area_changes(), 1);
            assert_eq!(session.state.own.pos(), arrival, "we stand where the server put us");
            assert!(session.info.fixture);
            assert!(session.area.max_speed() > hall_speed, "local movement now follows the yard's own rules");
            // ...and the snapshot of the new area, which the server sends after our answer, is accepted.
            pump_until(&mut session, "the yard's snapshot", |s| !s.state.npc_views().is_empty());
            let npcs: Vec<String> = session.state.npc_views().iter().map(|n| n.name.clone()).collect();
            session.leave(Duration::from_secs(2));
            npcs
        });
        let npcs = client.await.expect("the client thread");
        assert_eq!(npcs, ["guard"]);
        let answers = server.await.expect("the server task");
        match &answers[..] {
            [ClientMsg::JoinArea { area: first, .. }, ClientMsg::JoinArea { area, fingerprint, provenance }] => {
                assert_eq!(first.key, HALL);
                // The answer is about the area offered, from OUR content (and our own provenance).
                let yard = info_of(YARD);
                assert_eq!((area, *fingerprint), (&yard.id, yard.fingerprint));
                assert_eq!(*provenance, yard.provenance.summary());
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn an_area_this_machine_does_not_hold_is_refused_and_the_way_back_is_not_a_change() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // The server offers the yard, gets "cannot", and sends the player back to the hall.
        let back = change_for(HALL, Pos::new(250.0, 0.0, 0.0));
        let server = tokio::spawn(serve(listener, vec![change_for(YARD, Pos::new(YARD_ARRIVAL.0, YARD_ARRIVAL.1, 0.0)), back]));
        let client = tokio::task::spawn_blocking(move || {
            let mut net = NetHandle::spawn(config(addr), Box::new(OnlyHall(RoomsAdapter::new()))).expect("net thread");
            let joined = net.wait_joined().expect("joined");
            let mut session = Session::new(net, joined).with_content(Arc::new(OnlyHall(RoomsAdapter::new())));
            pump_until(&mut session, "the way back", |s| s.take_area_change().is_some());
            let (area, changes, pos) = (session.info.area_id.key.clone(), session.state.area_changes(), session.state.own.pos());
            session.leave(Duration::from_secs(2));
            (area, changes, pos)
        });
        let (area, changes, pos) = client.await.expect("the client thread");
        assert_eq!((area.as_str(), changes), (HALL, 0), "we never left the hall: the way back is not a change");
        assert_eq!((pos.x, pos.y), (250.0, 0.0));
        let answers = server.await.expect("the server task");
        match &answers[..] {
            [ClientMsg::JoinArea { .. }, ClientMsg::CannotJoin { area, reason }, ClientMsg::JoinArea { area: again, .. }] => {
                assert_eq!(area.key, YARD);
                assert!(reason.contains("yard"), "the reason says which area: {reason}");
                assert_eq!(again.key, HALL);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_hostile_or_missing_own_entity_still_gives_a_printable_identity() {
        // Server text is cleaned: no escape sequences or forged lines reach the title or the log.
        let hostile = [entity(7, "\u{1b}]0;PWNED\u{7}Cl\u{200b}oud\nSMOKE", [4, 5, 6])];
        let (name, tint) = own_identity(&hostile, EntityId(7));
        assert!(!name.chars().any(char::is_control), "{name:?}");
        assert!(name.starts_with("]0;PWNED") && name.contains("Cloud"), "{name:?}");
        assert_eq!(tint, [4, 5, 6]);
        // A name that is nothing visible, and an entity the snapshot does not contain.
        assert_eq!(own_identity(&[entity(7, "\u{200b}", [1, 1, 1])], EntityId(7)).0, "?");
        assert_eq!(own_identity(&[], EntityId(7)), ("?".to_string(), [255, 255, 255]));
        assert_eq!(own_identity(&[entity(1, "x", [0, 0, 0])], EntityId(7)).0, "?");
    }
}
