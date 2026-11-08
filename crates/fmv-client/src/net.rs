//! Networking: a tokio runtime on a background thread, talking to the game loop over channels.
//!
//! * [`handshake`] is generic over the reader/writer so it can be tested with in-memory pipes.
//!   It tolerates `Welcome` / `AreaManifest` / `AreaSnapshot` arriving in any order, treats
//!   `Reject` as fatal at every stage and bounds everything by one overall deadline.
//! * After the handshake a reader task forwards server messages and a writer loop sends
//!   `MoveIntent`s, a `Ping` every second and finally `Leave`. The reader is a separate task
//!   because `read_msg` is not cancel-safe and must never be dropped inside `select!`.
//! * The session is also a liveness check: the server answers every `Ping`, so a connection that
//!   stays completely silent for [`SessionTiming::silence_limit`] is declared dead even though
//!   the socket is still open (a stopped or black-holed server never closes it).
//! * No asset bytes are ever sent or received: only ids, fingerprints and path-free provenance.
//! * Text that comes from the server (including the text of decoding errors, which echoes what
//!   the server sent) is cleaned before it is shown, and local-only detail (adapter errors may
//!   name file paths) never goes onto the wire.

use crate::cli::exit;
use crate::state::Intent;
use fmv_adapter_api::{AreaRuntime, GameAdapter};
use fmv_core::{AreaId, EntityId, EntitySnapshot, Fingerprint, GameId, ProvenanceSummary, SessionId};
use fmv_protocol::{read_msg, write_msg, ClientMsg, ProtoError, ServerMsg, PROTOCOL_VERSION};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc::{self, error::TryRecvError};
use tokio::time::{timeout, timeout_at, Instant};

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// Server messages queued for the game loop (the reader waits when it is full). Bounded so a
/// flooding server cannot make the client buffer more than a few tens of MiB.
const EVENT_QUEUE: usize = 256;
/// Messages buffered while the handshake is still completing.
const MAX_BACKLOG: usize = 256;
/// A Pong older than this is ignored (bogus nonce).
const MAX_RTT: Duration = Duration::from_secs(60);

/// Timing of a running session. [`SessionTiming::DEFAULT`] is what the client uses; tests inject
/// short values so liveness can be checked in a fraction of a second.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionTiming {
    /// How often a `Ping` is sent.
    pub ping_interval: Duration,
    /// The server is declared gone after this long without a single inbound frame. The server
    /// answers every ping, so at the default (10 pings) a healthy connection is never this quiet.
    pub silence_limit: Duration,
}

impl SessionTiming {
    pub const DEFAULT: Self = Self {
        ping_interval: Duration::from_secs(1),
        silence_limit: Duration::from_secs(10),
    };
}

#[derive(Debug, Clone)]
pub struct NetConfig {
    pub server: String,
    pub display_name: String,
    pub tint: [u8; 3],
    pub appearance_ref: Option<String>,
    pub client_build: String,
    pub timing: SessionTiming,
}

#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("cannot connect to {addr}: {source} (is the server running?)")]
    Connect { addr: String, source: std::io::Error },
    #[error("connecting to {0} timed out after {1} s")]
    ConnectTimeout(String, u64),
    #[error("server rejected the connection: {0}")]
    Rejected(String),
    #[error("timed out waiting for {0} from the server")]
    Timeout(&'static str),
    #[error("protocol error: {0}")]
    Protocol(String),
    /// `detail` is already cleaned (see [`describe_proto_error`]); it never echoes server bytes.
    #[error("connection error while {during}: {detail}")]
    Io { during: &'static str, detail: String },
    /// Local content is missing or does not match the server's area; `CannotJoin` was sent.
    #[error("cannot join: {0}")]
    Content(String),
    #[error("network thread failed: {0}")]
    Runtime(String),
}

impl NetError {
    pub fn exit_code(&self) -> u8 {
        match self {
            NetError::Content(_) => exit::CONTENT,
            _ => exit::FAILURE,
        }
    }
}

/// Everything the game loop needs once the area has been agreed.
pub struct Joined {
    pub session: SessionId,
    pub entity: EntityId,
    pub server_build: String,
    pub area_id: AreaId,
    pub area_name: String,
    pub fixture: bool,
    pub provenance: ProvenanceSummary,
    /// The area as decoded from OUR OWN local content (never from the network).
    pub area: Box<dyn AreaRuntime>,
    /// Entities of the first `AreaSnapshot` (to find our spawn position).
    pub entities: Vec<EntitySnapshot>,
    /// Every post-manifest server message received during the handshake, in arrival order
    /// (includes the `AreaSnapshot`). The game loop applies them in order.
    pub backlog: Vec<ServerMsg>,
}

/// Messages from the game loop to the network thread.
#[derive(Debug, Clone, Copy)]
pub enum NetCommand {
    Intent(Intent),
    Leave,
}

/// Messages from the network thread to the game loop.
pub enum NetEvent {
    Joined(Box<Joined>),
    Failed(NetError),
    Server(ServerMsg),
    Rtt(Duration),
    /// The connection ended (EOF, I/O error, write timeout).
    Closed(String),
}

// ---------------------------------------------------------------------------------------------
// Local content check
// ---------------------------------------------------------------------------------------------

fn short(f: &Fingerprint) -> String {
    f.to_hex().chars().take(16).collect()
}

/// Longest server-provided text that is shown to the user.
const MAX_TEXT: usize = 200;
/// Longest local adapter error text shown on this machine (it never goes on the wire).
const MAX_LOCAL_DETAIL: usize = 400;

/// Characters that are not `char::is_control` but still forge or reorder terminal output: line and
/// paragraph separators (they split a line for many log parsers) and the bidirectional overrides
/// and zero-width marks that make text read differently from how it is stored.
fn is_deceptive(c: char) -> bool {
    matches!(
        c,
        '\u{2028}' | '\u{2029}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}'
    )
}

fn clean_to(s: &str, max: usize) -> String {
    s.chars().filter(|c| !c.is_control() && !is_deceptive(*c)).take(max).collect()
}

/// Server-provided text is untrusted: drop control characters (terminal escape sequences,
/// newlines) and deceptive separators, and clamp the length before it reaches a terminal, a log
/// or a window title.
pub fn clean(s: &str) -> String {
    clean_to(s, MAX_TEXT)
}

/// A short description of a transport error that is safe to print. Decoding errors are NOT
/// rendered with their own text: serde echoes the unknown enum variant or string it choked on,
/// which is whatever the server chose to send. Only the error category and column are kept.
pub fn describe_proto_error(e: &ProtoError) -> String {
    match e {
        ProtoError::Closed => "the server closed the connection".to_string(),
        ProtoError::Io(io) => format!("i/o error: {}", clean(&io.to_string())),
        ProtoError::FrameTooLarge(n) => format!("the server sent an oversized frame ({n} bytes)"),
        ProtoError::Codec(c) => {
            // `Category` is a fixed set of variant names (Io, Syntax, Data, Eof): no server text.
            let kind = format!("{:?}", c.classify()).to_lowercase();
            format!("the server sent a malformed message ({kind} error at column {})", c.column())
        }
    }
}

/// Why local content cannot serve the area the server offered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentMismatch {
    /// Sent to the server in `CannotJoin`. Only ids, fingerprints and path-free provenance.
    pub wire: String,
    /// Shown on this machine only. May carry adapter error text, which can name local paths.
    pub local: String,
}

impl ContentMismatch {
    /// A reason that is the same for the server and for the user.
    fn same(reason: String) -> Self {
        Self {
            wire: reason.clone(),
            local: reason,
        }
    }
}

/// Compare what the server offers with what this client holds locally. On success returns the
/// area decoded from local content; on failure a concrete reason for `CannotJoin` (and the
/// fuller one for the user).
pub fn check_local_content(
    adapter: &dyn GameAdapter,
    area: &AreaId,
    fingerprint: &Fingerprint,
    provenance: &ProvenanceSummary,
) -> Result<Box<dyn AreaRuntime>, ContentMismatch> {
    let local_game = adapter.game();
    // Everything the server supplied is cleaned before it is interpolated into a message that
    // is printed locally and sent back in `CannotJoin`.
    let key = clean(&area.key);
    if area.game != local_game {
        return Err(ContentMismatch::same(if area.game == GameId::Ff7 {
            format!(
                "server offers FINAL FANTASY VII area `{key}`: needs owned FF7 content imported via fmv-import (this client only has {})",
                local_game.label()
            )
        } else {
            format!(
                "server offers {} area `{key}`: needs owned content for that game imported via fmv-import (this client only has {})",
                area.game.label(),
                local_game.label()
            )
        }));
    }
    let areas = adapter.list_areas();
    let Some(info) = areas.iter().find(|a| a.id == *area) else {
        let have: Vec<&str> = areas.iter().take(8).map(|a| a.id.key.as_str()).collect();
        return Err(ContentMismatch::same(format!(
            "local {} content has no area `{key}` (local areas: {})",
            local_game.label(),
            have.join(", ")
        )));
    };
    if info.fingerprint != *fingerprint {
        return Err(ContentMismatch::same(format!(
            "area `{key}` fingerprint mismatch: server {}, local {} (different edition, region or revision)",
            short(fingerprint),
            short(&info.fingerprint)
        )));
    }
    let local = info.provenance.summary();
    if !local.compatible_with(provenance) {
        return Err(ContentMismatch::same(format!(
            "area `{key}` edition mismatch: server {}/{}/{}, local {}/{}/{}",
            clean(&provenance.edition),
            clean(&provenance.region),
            clean(&provenance.revision),
            local.edition,
            local.region,
            local.revision
        )));
    }
    let runtime = adapter.load_area(&area.key).map_err(|e| ContentMismatch {
        // An adapter error may name a file on this machine: it stays on this machine.
        wire: format!("local content for area `{key}` could not be loaded"),
        local: format!(
            "cannot load local area `{key}`: {}",
            clean_to(&e.to_string(), MAX_LOCAL_DETAIL)
        ),
    })?;
    if runtime.info().fingerprint != *fingerprint {
        return Err(ContentMismatch::same(format!(
            "local area `{key}` decoded to a different fingerprint than its catalog entry"
        )));
    }
    Ok(runtime)
}

// ---------------------------------------------------------------------------------------------
// Handshake
// ---------------------------------------------------------------------------------------------

async fn send<W: AsyncWrite + Unpin>(
    wr: &mut W,
    msg: &ClientMsg,
    during: &'static str,
    deadline: Instant,
) -> Result<(), NetError> {
    match timeout_at(deadline, write_msg(wr, msg)).await {
        Err(_) => Err(NetError::Timeout(during)),
        Ok(r) => r.map_err(|source| NetError::Io {
            during,
            detail: describe_proto_error(&source),
        }),
    }
}

struct Welcome {
    session: SessionId,
    entity: EntityId,
    server_build: String,
}

struct Offer {
    area: AreaId,
    display_name: String,
    fixture: bool,
    provenance: ProvenanceSummary,
    runtime: Box<dyn AreaRuntime>,
}

/// Run the client side of the handshake:
/// `Hello` -> (`Welcome`, `AreaManifest`) -> local content check -> `JoinArea` or `CannotJoin`
/// -> `AreaSnapshot`.
pub async fn handshake<R, W>(
    rd: &mut R,
    wr: &mut W,
    cfg: &NetConfig,
    adapter: &dyn GameAdapter,
    budget: Duration,
) -> Result<Joined, NetError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let deadline = Instant::now() + budget;
    let hello = ClientMsg::Hello {
        protocol_version: PROTOCOL_VERSION,
        client_build: cfg.client_build.clone(),
        display_name: cfg.display_name.clone(),
        tint: cfg.tint,
        appearance_ref: cfg.appearance_ref.clone(),
    };
    send(wr, &hello, "sending Hello", deadline).await?;

    let mut welcome: Option<Welcome> = None;
    let mut offer: Option<Offer> = None;
    let mut snapshot: Option<Vec<EntitySnapshot>> = None;
    let mut backlog: Vec<ServerMsg> = Vec::new();

    loop {
        let waiting = if welcome.is_none() {
            "Welcome"
        } else if offer.is_none() {
            "AreaManifest"
        } else {
            "AreaSnapshot"
        };
        let msg: ServerMsg = match timeout_at(deadline, read_msg(rd)).await {
            Err(_) => return Err(NetError::Timeout(waiting)),
            Ok(Err(source)) => {
                return Err(NetError::Io {
                    during: waiting,
                    detail: describe_proto_error(&source),
                })
            }
            Ok(Ok(m)) => m,
        };
        match msg {
            ServerMsg::Reject { reason } => return Err(NetError::Rejected(clean(&reason))),
            ServerMsg::Welcome {
                protocol_version,
                server_build,
                session,
                entity,
            } => {
                if protocol_version != PROTOCOL_VERSION {
                    return Err(NetError::Protocol(format!(
                        "server speaks protocol v{protocol_version}, this client speaks v{PROTOCOL_VERSION}"
                    )));
                }
                if welcome.is_some() {
                    return Err(NetError::Protocol("duplicate Welcome".into()));
                }
                welcome = Some(Welcome {
                    session,
                    entity,
                    server_build,
                });
            }
            ServerMsg::AreaManifest {
                area,
                display_name,
                fingerprint,
                provenance,
                fixture,
            } => {
                if offer.is_some() {
                    return Err(NetError::Protocol("duplicate AreaManifest".into()));
                }
                match check_local_content(adapter, &area, &fingerprint, &provenance) {
                    Ok(runtime) => {
                        let join = ClientMsg::JoinArea {
                            area: area.clone(),
                            fingerprint,
                        };
                        send(wr, &join, "sending JoinArea", deadline).await?;
                        // FIXTURE is judged from OUR content as well as the server's flag, so
                        // synthetic content can never be presented as real by a wrong flag.
                        let fixture = fixture || runtime.info().fixture || area.game.is_fixture();
                        offer = Some(Offer {
                            area,
                            display_name,
                            fixture,
                            provenance,
                            runtime,
                        });
                    }
                    Err(mismatch) => {
                        // Tell the server why (path-free text only); a failed send is irrelevant
                        // because we are leaving. The user sees the fuller local reason.
                        let cannot = ClientMsg::CannotJoin {
                            area,
                            reason: mismatch.wire,
                        };
                        let _ = send(wr, &cannot, "sending CannotJoin", deadline).await;
                        let _ = wr.shutdown().await;
                        return Err(NetError::Content(mismatch.local));
                    }
                }
            }
            ServerMsg::AreaSnapshot { area, tick, entities } => {
                let Some(o) = &offer else {
                    return Err(NetError::Protocol("AreaSnapshot before AreaManifest".into()));
                };
                if area != o.area {
                    return Err(NetError::Protocol(format!(
                        "AreaSnapshot for `{}` but the offered area is `{}`",
                        clean(&area.to_string()),
                        o.area
                    )));
                }
                if snapshot.is_none() {
                    snapshot = Some(entities.clone());
                }
                push_backlog(&mut backlog, ServerMsg::AreaSnapshot { area, tick, entities });
            }
            ServerMsg::Pong { .. } => {}
            other @ (ServerMsg::EntityUpdate { .. }
            | ServerMsg::PlayerJoined { .. }
            | ServerMsg::PlayerLeft { .. }
            | ServerMsg::MoveRejected { .. }) => {
                if offer.is_some() {
                    push_backlog(&mut backlog, other);
                }
            }
        }
        if let (Some(_), Some(_), Some(_)) = (&welcome, &offer, &snapshot) {
            let (Some(w), Some(o), Some(entities)) = (welcome, offer, snapshot) else {
                return Err(NetError::Protocol("handshake state lost".into()));
            };
            return Ok(Joined {
                session: w.session,
                entity: w.entity,
                server_build: clean(&w.server_build),
                area_id: o.area,
                area_name: clean(&o.display_name),
                fixture: o.fixture,
                provenance: o.provenance,
                area: o.runtime,
                entities,
                backlog,
            });
        }
    }
}

fn push_backlog(backlog: &mut Vec<ServerMsg>, msg: ServerMsg) {
    if backlog.len() < MAX_BACKLOG {
        backlog.push(msg);
    }
}

// ---------------------------------------------------------------------------------------------
// Steady state
// ---------------------------------------------------------------------------------------------

/// RTT from a pong nonce. Pings carry the microseconds since the connection's origin, so no
/// state is needed; a nonce from the future or from a minute ago is ignored.
pub fn rtt_from_nonce(elapsed: Duration, nonce: u64) -> Option<Duration> {
    let now_us = u64::try_from(elapsed.as_micros()).ok()?;
    let rtt = Duration::from_micros(now_us.checked_sub(nonce)?);
    (rtt <= MAX_RTT).then_some(rtt)
}

/// Forward server messages to the game loop. Ends with exactly one `NetEvent::Closed` when the
/// connection ends, fails, or stays silent for `silence_limit`.
///
/// Each read is bounded by `silence_limit`. Dropping `read_msg` mid-frame is not cancel-safe, but
/// when the limit fires the session is over anyway and the reader is never used again. The time
/// the reader spends waiting for the game loop to take an event (a full queue) is not counted.
async fn reader_task<R: AsyncRead + Unpin>(
    mut rd: R,
    evt_tx: mpsc::Sender<NetEvent>,
    origin: Instant,
    silence_limit: Duration,
) {
    loop {
        let event = match timeout(silence_limit, read_msg::<_, ServerMsg>(&mut rd)).await {
            Err(_) => {
                let why = format!(
                    "the server stopped responding (no data for {} s)",
                    silence_limit.as_secs_f64()
                );
                let _ = evt_tx.send(NetEvent::Closed(why)).await;
                return;
            }
            Ok(Ok(ServerMsg::Pong { nonce, .. })) => match rtt_from_nonce(origin.elapsed(), nonce) {
                Some(rtt) => NetEvent::Rtt(rtt),
                None => continue,
            },
            Ok(Ok(msg)) => NetEvent::Server(msg),
            Ok(Err(e)) => {
                // Never `e.to_string()`: a decoding error echoes what the server sent.
                let _ = evt_tx.send(NetEvent::Closed(describe_proto_error(&e))).await;
                return;
            }
        };
        if evt_tx.send(event).await.is_err() {
            return; // the game loop is gone
        }
    }
}

async fn session_loop<R, W>(
    rd: R,
    mut wr: W,
    mut cmd_rx: mpsc::UnboundedReceiver<NetCommand>,
    evt_tx: mpsc::Sender<NetEvent>,
    origin: Instant,
    timing: SessionTiming,
) where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin,
{
    let mut reader = tokio::spawn(reader_task(rd, evt_tx.clone(), origin, timing.silence_limit));
    let mut ping = tokio::time::interval(timing.ping_interval);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        let msg = tokio::select! {
            cmd = cmd_rx.recv() => match cmd {
                Some(NetCommand::Intent(i)) => ClientMsg::MoveIntent { seq: i.seq, pos: i.pos, facing: i.facing, anim: i.anim },
                Some(NetCommand::Leave) | None => {
                    let _ = timeout(WRITE_TIMEOUT, write_msg(&mut wr, &ClientMsg::Leave)).await;
                    break;
                }
            },
            _ = ping.tick() => {
                let nonce = u64::try_from(origin.elapsed().as_micros()).unwrap_or(u64::MAX);
                ClientMsg::Ping { nonce }
            },
            _ = &mut reader => break, // the reader already reported why
        };
        match timeout(WRITE_TIMEOUT, write_msg(&mut wr, &msg)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                let why = format!("write failed: {}", describe_proto_error(&e));
                let _ = evt_tx.send(NetEvent::Closed(why)).await;
                break;
            }
            Err(_) => {
                let _ = evt_tx
                    .send(NetEvent::Closed("the server stopped reading (write timed out)".into()))
                    .await;
                break;
            }
        }
    }
    let _ = timeout(WRITE_TIMEOUT, wr.shutdown()).await;
    reader.abort();
}

async fn connect(server: &str) -> Result<TcpStream, NetError> {
    match timeout(CONNECT_TIMEOUT, TcpStream::connect(server)).await {
        Err(_) => Err(NetError::ConnectTimeout(server.to_string(), CONNECT_TIMEOUT.as_secs())),
        Ok(Err(source)) => Err(NetError::Connect {
            addr: server.to_string(),
            source,
        }),
        Ok(Ok(s)) => {
            let _ = s.set_nodelay(true);
            Ok(s)
        }
    }
}

async fn net_main(
    cfg: NetConfig,
    adapter: Box<dyn GameAdapter>,
    cmd_rx: mpsc::UnboundedReceiver<NetCommand>,
    evt_tx: mpsc::Sender<NetEvent>,
) {
    let origin = Instant::now();
    let stream = match connect(&cfg.server).await {
        Ok(s) => s,
        Err(e) => {
            let _ = evt_tx.send(NetEvent::Failed(e)).await;
            return;
        }
    };
    let (mut rd, mut wr) = stream.into_split();
    match handshake(&mut rd, &mut wr, &cfg, adapter.as_ref(), HANDSHAKE_TIMEOUT).await {
        Ok(joined) => {
            if evt_tx.send(NetEvent::Joined(Box::new(joined))).await.is_err() {
                return;
            }
            session_loop(rd, wr, cmd_rx, evt_tx, origin, cfg.timing).await;
        }
        Err(e) => {
            let _ = evt_tx.send(NetEvent::Failed(e)).await;
        }
    }
}

fn net_thread(
    cfg: NetConfig,
    adapter: Box<dyn GameAdapter>,
    cmd_rx: mpsc::UnboundedReceiver<NetCommand>,
    evt_tx: mpsc::Sender<NetEvent>,
) {
    match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt.block_on(net_main(cfg, adapter, cmd_rx, evt_tx)),
        Err(e) => {
            let _ = evt_tx.blocking_send(NetEvent::Failed(NetError::Runtime(format!(
                "cannot start the async runtime: {e}"
            ))));
        }
    }
}

/// Handle to the network thread, owned by the game loop.
pub struct NetHandle {
    cmd_tx: mpsc::UnboundedSender<NetCommand>,
    events: mpsc::Receiver<NetEvent>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// We asked the network thread to say goodbye: its ending is expected.
    leaving: bool,
    /// A `Closed` event (real or synthesized) was already handed out.
    closed_reported: bool,
}

impl NetHandle {
    /// Start the network thread: connect, run the handshake, then serve the session.
    pub fn spawn(cfg: NetConfig, adapter: Box<dyn GameAdapter>) -> std::io::Result<Self> {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (evt_tx, events) = mpsc::channel(EVENT_QUEUE);
        let thread = std::thread::Builder::new()
            .name("fmv-net".into())
            .spawn(move || net_thread(cfg, adapter, cmd_rx, evt_tx))?;
        Ok(Self {
            cmd_tx,
            events,
            thread: Some(thread),
            leaving: false,
            closed_reported: false,
        })
    }

    /// Block until the handshake finished (the network thread enforces its own timeouts).
    pub fn wait_joined(&mut self) -> Result<Joined, NetError> {
        match self.events.blocking_recv() {
            Some(NetEvent::Joined(j)) => Ok(*j),
            Some(NetEvent::Failed(e)) => Err(e),
            Some(_) => Err(NetError::Protocol("unexpected event before the area was joined".into())),
            None => Err(NetError::Runtime("the network thread ended unexpectedly".into())),
        }
    }

    pub fn send(&self, cmd: NetCommand) {
        let _ = self.cmd_tx.send(cmd);
    }

    /// The next queued event, or `None` when there is none right now. If the network thread is
    /// gone without having said why (it panicked) and we did not ask it to leave, a single
    /// `Closed` event is produced so the game loop ends the session instead of drawing a frozen
    /// world forever.
    pub fn try_next(&mut self) -> Option<NetEvent> {
        match self.events.try_recv() {
            Ok(ev) => {
                if matches!(ev, NetEvent::Closed(_)) {
                    self.closed_reported = true;
                }
                Some(ev)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                if self.leaving || self.closed_reported {
                    return None;
                }
                self.closed_reported = true;
                Some(NetEvent::Closed("the network thread ended unexpectedly".into()))
            }
        }
    }

    /// Send `Leave` and wait (bounded) for the network thread to flush it and exit.
    pub fn leave_and_join(&mut self, wait: Duration) {
        // Before anything else: from now on the thread ending is expected, not a failure.
        self.leaving = true;
        self.send(NetCommand::Leave);
        let Some(thread) = self.thread.take() else { return };
        let start = std::time::Instant::now();
        while !thread.is_finished() && start.elapsed() < wait {
            std::thread::sleep(Duration::from_millis(5));
        }
        if thread.is_finished() {
            let _ = thread.join();
        }
        // Otherwise detach: the process is exiting and the thread dies with it.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_adapter_api::{AdapterError, AreaInfo};
    use fmv_core::{AnimState, AvatarAppearance, Pos};
    use fmv_fixture::FixtureAdapter;
    use tokio::io::{duplex, split};

    fn cfg() -> NetConfig {
        NetConfig {
            server: "unused".into(),
            display_name: "tester".into(),
            tint: [1, 2, 3],
            appearance_ref: None,
            client_build: "test".into(),
            timing: SessionTiming::DEFAULT,
        }
    }

    /// Short timings for the liveness tests (they run on tokio's paused clock).
    fn fast_timing() -> SessionTiming {
        SessionTiming {
            ping_interval: Duration::from_millis(50),
            silence_limit: Duration::from_millis(400),
        }
    }

    /// Catalog identical to the fixture's, but the content cannot be loaded and the error names a
    /// file on this machine (as a real adapter's "missing file" error would).
    struct BrokenAdapter(FixtureAdapter);

    const SECRET_PATH: &str = "/home/secret/ff7/data/flevel.lgp";

    impl GameAdapter for BrokenAdapter {
        fn game(&self) -> GameId {
            self.0.game()
        }
        fn list_areas(&self) -> Vec<AreaInfo> {
            self.0.list_areas()
        }
        fn load_area(&self, _key: &str) -> Result<Box<dyn AreaRuntime>, AdapterError> {
            Err(AdapterError::MissingContent(format!("flevel.lgp not found at {SECRET_PATH}")))
        }
    }

    /// A frame (length prefix + payload) whose JSON object has one hostile enum-variant key.
    /// serde's "unknown variant" error echoes that key, i.e. text chosen by the server.
    fn hostile_frame() -> Vec<u8> {
        let name = format!(
            "Welcome\\u001b]0;PWNED-TITLE\\u0007\\u001b[2J\\nSMOKE name=forged area=Fixture/plaza {}",
            "x".repeat(5000)
        );
        let payload = format!("{{\"{name}\":{{}}}}").into_bytes();
        let mut frame = u32::try_from(payload.len()).unwrap().to_le_bytes().to_vec();
        frame.extend_from_slice(&payload);
        frame
    }

    fn assert_printable_and_short(text: &str) {
        assert!(!text.chars().any(|c| c.is_control() || is_deceptive(c)), "{text:?}");
        assert!(text.chars().count() <= 200, "{} chars: {text:?}", text.chars().count());
        for echoed in ["PWNED", "forged", "SMOKE", "xxxx"] {
            assert!(!text.contains(echoed), "server text was echoed ({echoed}): {text:?}");
        }
    }

    fn manifest(adapter: &FixtureAdapter, tamper: impl FnOnce(&mut ServerMsg)) -> ServerMsg {
        let info = adapter.list_areas().remove(0);
        let mut m = ServerMsg::AreaManifest {
            area: info.id,
            display_name: info.display_name,
            fingerprint: info.fingerprint,
            provenance: info.provenance.summary(),
            fixture: info.fixture,
        };
        tamper(&mut m);
        m
    }

    fn welcome() -> ServerMsg {
        ServerMsg::Welcome {
            protocol_version: PROTOCOL_VERSION,
            server_build: "srv".into(),
            session: SessionId(5),
            entity: EntityId(7),
        }
    }

    fn snap(id: u64, x: f32) -> EntitySnapshot {
        EntitySnapshot {
            id: EntityId(id),
            pos: Pos::new(x, 0.0, 0.0),
            facing: 0.0,
            anim: AnimState::Idle,
            avatar: AvatarAppearance {
                display_name: format!("p{id}"),
                tint: [9, 9, 9],
                appearance_ref: None,
            },
        }
    }

    fn area_snapshot(adapter: &FixtureAdapter, entities: Vec<EntitySnapshot>) -> ServerMsg {
        ServerMsg::AreaSnapshot {
            area: adapter.list_areas().remove(0).id,
            tick: 3,
            entities,
        }
    }

    #[tokio::test]
    async fn handshake_tolerates_manifest_before_welcome_and_keeps_the_backlog_in_order() {
        let adapter = FixtureAdapter::new();
        let (client, server) = duplex(64 * 1024);
        let (mut crd, mut cwr) = split(client);
        let (mut srd, mut swr) = split(server);
        let srv_adapter = FixtureAdapter::new();
        let server_task = tokio::spawn(async move {
            let hello: ClientMsg = read_msg(&mut srd).await.unwrap();
            let ClientMsg::Hello {
                protocol_version,
                display_name,
                tint,
                ..
            } = hello
            else {
                panic!("expected Hello")
            };
            assert_eq!(
                (protocol_version, display_name.as_str(), tint),
                (PROTOCOL_VERSION, "tester", [1, 2, 3])
            );
            // Unusual order: manifest first, welcome only after the client joined.
            write_msg(&mut swr, &manifest(&srv_adapter, |_| {})).await.unwrap();
            let join: ClientMsg = read_msg(&mut srd).await.unwrap();
            assert!(matches!(join, ClientMsg::JoinArea { .. }), "{join:?}");
            write_msg(&mut swr, &ServerMsg::Pong { nonce: 1, tick: 1 })
                .await
                .unwrap();
            write_msg(&mut swr, &welcome()).await.unwrap();
            write_msg(&mut swr, &ServerMsg::PlayerJoined { snapshot: snap(8, 1.0) })
                .await
                .unwrap();
            write_msg(&mut swr, &area_snapshot(&srv_adapter, vec![snap(7, 0.0), snap(8, 1.0)]))
                .await
                .unwrap();
        });
        let joined = handshake(&mut crd, &mut cwr, &cfg(), &adapter, Duration::from_secs(5))
            .await
            .unwrap();
        server_task.await.unwrap();
        assert_eq!((joined.session, joined.entity), (SessionId(5), EntityId(7)));
        assert_eq!(joined.server_build, "srv");
        assert!(joined.fixture);
        assert_eq!(joined.area_id.key, "plaza");
        assert_eq!(joined.entities.len(), 2);
        assert_eq!(joined.backlog.len(), 2);
        assert!(matches!(joined.backlog[0], ServerMsg::PlayerJoined { .. }));
        assert!(matches!(joined.backlog[1], ServerMsg::AreaSnapshot { .. }));
        assert_eq!(joined.area.info().fingerprint, adapter.list_areas()[0].fingerprint);
    }

    #[tokio::test]
    async fn fingerprint_mismatch_sends_cannot_join_and_fails_with_a_content_error() {
        let adapter = FixtureAdapter::new();
        let (client, server) = duplex(64 * 1024);
        let (mut crd, mut cwr) = split(client);
        let (mut srd, mut swr) = split(server);
        let srv_adapter = FixtureAdapter::new();
        let server_task = tokio::spawn(async move {
            let _hello: ClientMsg = read_msg(&mut srd).await.unwrap();
            write_msg(&mut swr, &welcome()).await.unwrap();
            let tampered = manifest(&srv_adapter, |m| {
                if let ServerMsg::AreaManifest { fingerprint, .. } = m {
                    fingerprint.0[0] ^= 0xff;
                }
            });
            write_msg(&mut swr, &tampered).await.unwrap();
            read_msg::<_, ClientMsg>(&mut srd).await.unwrap()
        });
        let err = handshake(&mut crd, &mut cwr, &cfg(), &adapter, Duration::from_secs(5))
            .await
            .err()
            .expect("must fail");
        assert!(
            matches!(&err, NetError::Content(r) if r.contains("fingerprint mismatch")),
            "{err}"
        );
        assert_eq!(err.exit_code(), exit::CONTENT);
        match server_task.await.unwrap() {
            ClientMsg::CannotJoin { area, reason } => {
                assert_eq!(area.key, "plaza");
                assert!(reason.contains("fingerprint mismatch"), "{reason}");
            }
            other => panic!("expected CannotJoin, got {other:?}"),
        }
    }

    #[test]
    fn content_check_reasons_are_concrete() {
        let adapter = FixtureAdapter::new();
        let info = adapter.list_areas().remove(0);
        let prov = info.provenance.summary();

        let ff7 = AreaId {
            game: GameId::Ff7,
            key: "md1stin".into(),
        };
        // Every reason except a load failure is the same text for the server and for the user.
        let reason = |m: ContentMismatch| {
            assert_eq!(m.wire, m.local, "only load errors may differ");
            m.local
        };
        let r = reason(
            check_local_content(&adapter, &ff7, &info.fingerprint, &prov)
                .err()
                .unwrap(),
        );
        assert!(r.contains("needs owned FF7 content imported via fmv-import"), "{r}");
        assert!(r.contains("md1stin"), "{r}");

        let other = AreaId {
            game: GameId::Ff9,
            key: "x".into(),
        };
        let r = reason(
            check_local_content(&adapter, &other, &info.fingerprint, &prov)
                .err()
                .unwrap(),
        );
        assert!(r.contains("fmv-import") && r.contains("FINAL FANTASY IX"), "{r}");

        let missing = AreaId {
            game: GameId::Fixture,
            key: "nowhere".into(),
        };
        let r = reason(
            check_local_content(&adapter, &missing, &info.fingerprint, &prov)
                .err()
                .unwrap(),
        );
        assert!(r.contains("no area `nowhere`") && r.contains("plaza"), "{r}");

        let mut other_edition = prov.clone();
        other_edition.region = "PAL".into();
        let r = reason(
            check_local_content(&adapter, &info.id, &info.fingerprint, &other_edition)
                .err()
                .unwrap(),
        );
        assert!(r.contains("edition mismatch"), "{r}");

        let mut bad = info.fingerprint;
        bad.0[31] ^= 1;
        let r = reason(check_local_content(&adapter, &info.id, &bad, &prov).err().unwrap());
        assert!(r.contains("fingerprint mismatch"), "{r}");

        assert!(check_local_content(&adapter, &info.id, &info.fingerprint, &prov).is_ok());
    }

    /// An adapter error can name a file on this machine. The user is told; the server is not.
    #[tokio::test]
    async fn local_paths_in_adapter_errors_never_reach_the_wire() {
        let broken = BrokenAdapter(FixtureAdapter::new());
        let info = broken.list_areas().remove(0);
        let m = check_local_content(&broken, &info.id, &info.fingerprint, &info.provenance.summary())
            .err()
            .expect("the content cannot be loaded");
        assert!(m.local.contains(SECRET_PATH), "the user sees the real reason: {}", m.local);
        assert!(!m.wire.contains(SECRET_PATH) && !m.wire.contains("/home"), "{}", m.wire);
        assert!(m.wire.contains("plaza") && m.wire.contains("could not be loaded"), "{}", m.wire);

        // Through the real handshake: what the server receives in `CannotJoin`.
        let (client, server) = duplex(64 * 1024);
        let (mut crd, mut cwr) = split(client);
        let (mut srd, mut swr) = split(server);
        let srv_adapter = FixtureAdapter::new();
        let server_task = tokio::spawn(async move {
            let _hello: ClientMsg = read_msg(&mut srd).await.unwrap();
            write_msg(&mut swr, &welcome()).await.unwrap();
            write_msg(&mut swr, &manifest(&srv_adapter, |_| {})).await.unwrap();
            read_msg::<_, ClientMsg>(&mut srd).await.unwrap()
        });
        let err = handshake(&mut crd, &mut cwr, &cfg(), &broken, Duration::from_secs(5))
            .await
            .err()
            .expect("must fail");
        assert!(matches!(&err, NetError::Content(r) if r.contains(SECRET_PATH)), "{err}");
        assert_eq!(err.exit_code(), exit::CONTENT);
        match server_task.await.unwrap() {
            ClientMsg::CannotJoin { reason, .. } => {
                assert!(!reason.contains(SECRET_PATH) && !reason.contains("/home"), "{reason}");
                assert!(reason.contains("could not be loaded"), "{reason}");
            }
            other => panic!("expected CannotJoin, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn reject_is_fatal_at_any_stage() {
        for stage in 0..3 {
            let adapter = FixtureAdapter::new();
            let (client, server) = duplex(64 * 1024);
            let (mut crd, mut cwr) = split(client);
            let (mut srd, mut swr) = split(server);
            let srv_adapter = FixtureAdapter::new();
            let server_task = tokio::spawn(async move {
                let _hello: ClientMsg = read_msg(&mut srd).await.unwrap();
                if stage >= 1 {
                    write_msg(&mut swr, &welcome()).await.unwrap();
                }
                if stage >= 2 {
                    write_msg(&mut swr, &manifest(&srv_adapter, |_| {})).await.unwrap();
                    let _join: ClientMsg = read_msg(&mut srd).await.unwrap();
                }
                write_msg(
                    &mut swr,
                    &ServerMsg::Reject {
                        reason: "server full".into(),
                    },
                )
                .await
                .unwrap();
            });
            let err = handshake(&mut crd, &mut cwr, &cfg(), &adapter, Duration::from_secs(5))
                .await
                .err()
                .expect("must fail");
            server_task.await.unwrap();
            assert!(
                matches!(&err, NetError::Rejected(r) if r == "server full"),
                "stage {stage}: {err}"
            );
            assert_eq!(err.exit_code(), exit::FAILURE);
        }
    }

    #[tokio::test]
    async fn protocol_violations_and_silence_fail_cleanly() {
        let adapter = FixtureAdapter::new();

        // Wrong protocol version.
        let (client, server) = duplex(4096);
        let (mut crd, mut cwr) = split(client);
        let (_srd, mut swr) = split(server);
        write_msg(
            &mut swr,
            &ServerMsg::Welcome {
                protocol_version: 99,
                server_build: "x".into(),
                session: SessionId(1),
                entity: EntityId(1),
            },
        )
        .await
        .unwrap();
        let err = handshake(&mut crd, &mut cwr, &cfg(), &adapter, Duration::from_secs(5))
            .await
            .err()
            .unwrap();
        assert!(matches!(&err, NetError::Protocol(m) if m.contains("v99")), "{err}");

        // A snapshot before any manifest.
        let (client, server) = duplex(4096);
        let (mut crd, mut cwr) = split(client);
        let (_srd, mut swr) = split(server);
        write_msg(&mut swr, &welcome()).await.unwrap();
        write_msg(&mut swr, &area_snapshot(&adapter, vec![])).await.unwrap();
        let err = handshake(&mut crd, &mut cwr, &cfg(), &adapter, Duration::from_secs(5))
            .await
            .err()
            .unwrap();
        assert!(
            matches!(&err, NetError::Protocol(m) if m.contains("before AreaManifest")),
            "{err}"
        );

        // Silence: a bounded wait that names what was missing.
        let (client, _server) = duplex(4096);
        let (mut crd, mut cwr) = split(client);
        let err = handshake(&mut crd, &mut cwr, &cfg(), &adapter, Duration::from_millis(150))
            .await
            .err()
            .unwrap();
        assert!(matches!(err, NetError::Timeout("Welcome")), "{err}");

        // The server hangs up mid-handshake.
        let (client, server) = duplex(4096);
        let (mut crd, mut cwr) = split(client);
        drop(server);
        let err = handshake(&mut crd, &mut cwr, &cfg(), &adapter, Duration::from_secs(5))
            .await
            .err()
            .unwrap();
        assert!(matches!(err, NetError::Io { .. }), "{err}");
    }

    #[test]
    fn server_text_is_cleaned_before_display() {
        assert_eq!(clean("a\u{1b}[31mb\nc\u{7}d"), "a[31mbcd");
        assert_eq!(clean(&"x".repeat(1000)).chars().count(), MAX_TEXT);
        let adapter = FixtureAdapter::new();
        let info = adapter.list_areas().remove(0);
        let hostile = AreaId {
            game: GameId::Fixture,
            key: "\u{1b}]0;pwned\u{7}evil\nSMOKE name=x".into(),
        };
        let m = check_local_content(&adapter, &hostile, &info.fingerprint, &info.provenance.summary())
            .err()
            .unwrap();
        for r in [&m.wire, &m.local] {
            assert!(!r.chars().any(char::is_control), "{r:?}");
        }
        // Line/paragraph separators and bidi overrides would also forge or reorder output.
        assert_eq!(clean("a\u{2028}b\u{2029}c\u{202e}d\u{200b}e\u{feff}f"), "abcdef");
        assert_eq!(clean("Zoë 日本語"), "Zoë 日本語", "ordinary text is untouched");
    }

    /// serde's decoding errors echo the text the server sent. In both phases that text must not
    /// reach stderr: no escape sequences, no forged lines, no 250 kB messages.
    #[tokio::test]
    async fn decoding_errors_never_echo_server_text() {
        // Steady state: the reader reports `Closed` with a fixed-shape description.
        let (mut srv, cli) = duplex(1 << 20);
        let (evt_tx, mut evt_rx) = mpsc::channel(8);
        let reader = tokio::spawn(reader_task(cli, evt_tx, Instant::now(), Duration::from_secs(5)));
        srv.write_all(&hostile_frame()).await.unwrap();
        let NetEvent::Closed(why) = evt_rx.recv().await.expect("an event") else {
            panic!("expected Closed")
        };
        assert!(why.contains("malformed message"), "still says what went wrong: {why}");
        assert_printable_and_short(&why);
        reader.await.unwrap();

        // Handshake: the same frame where `Welcome` is expected.
        let adapter = FixtureAdapter::new();
        let (client, server) = duplex(1 << 20);
        let (mut crd, mut cwr) = split(client);
        let (mut srd, mut swr) = split(server);
        let server_task = tokio::spawn(async move {
            let _hello: ClientMsg = read_msg(&mut srd).await.unwrap();
            swr.write_all(&hostile_frame()).await.unwrap();
        });
        let err = handshake(&mut crd, &mut cwr, &cfg(), &adapter, Duration::from_secs(5))
            .await
            .err()
            .expect("must fail");
        server_task.await.unwrap();
        assert!(matches!(err, NetError::Io { .. }), "{err}");
        assert_printable_and_short(&err.to_string());

        // Other transport errors keep their useful (and fixed or numeric) description.
        assert_eq!(
            describe_proto_error(&ProtoError::Closed),
            "the server closed the connection"
        );
        assert!(describe_proto_error(&ProtoError::FrameTooLarge(9_999_999)).contains("9999999"));
    }

    /// A server that stops answering without closing the socket (SIGSTOP, a black-holing
    /// network) is declared gone after the silence limit; one that keeps answering is not.
    /// Runs on tokio's paused clock, so it is exact and independent of machine load.
    #[tokio::test(start_paused = true)]
    async fn a_silent_server_is_declared_gone_but_a_live_one_is_not() {
        // --- Silent: the pipe stays open, nothing is ever sent back.
        let (client, server) = duplex(64 * 1024);
        let (crd, cwr) = split(client);
        let (_srd, _swr) = split(server); // held open, never read, never written
        let (_cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (evt_tx, mut evt_rx) = mpsc::channel(64);
        let started = Instant::now();
        let session = tokio::spawn(session_loop(crd, cwr, cmd_rx, evt_tx, started, fast_timing()));
        // (Virtual time: a regression fails here instead of spinning on the ping timer forever.)
        let first = timeout(Duration::from_secs(60), evt_rx.recv())
            .await
            .expect("a silent server must be reported within a minute")
            .expect("an event");
        let NetEvent::Closed(why) = first else { panic!("expected Closed") };
        assert!(why.contains("stopped responding"), "{why}");
        let waited = started.elapsed();
        assert!(
            waited >= fast_timing().silence_limit && waited < fast_timing().silence_limit * 2,
            "reported after {waited:?}"
        );
        timeout(Duration::from_secs(60), session)
            .await
            .expect("the session loop must end by itself")
            .unwrap();

        // --- Alive: the server answers every Ping, so the session outlives 3x the limit.
        let (client, server) = duplex(64 * 1024);
        let (crd, cwr) = split(client);
        let (mut srd, mut swr) = split(server);
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (evt_tx, mut evt_rx) = mpsc::channel(256);
        let server_task = tokio::spawn(async move {
            let mut pings = 0u32;
            loop {
                match read_msg::<_, ClientMsg>(&mut srd).await {
                    Ok(ClientMsg::Ping { nonce }) => {
                        pings += 1;
                        let pong = ServerMsg::Pong { nonce, tick: 1 };
                        if write_msg(&mut swr, &pong).await.is_err() {
                            break;
                        }
                    }
                    Ok(ClientMsg::Leave) => return (pings, true),
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            (pings, false)
        });
        let session = tokio::spawn(session_loop(crd, cwr, cmd_rx, evt_tx, Instant::now(), fast_timing()));
        tokio::time::sleep(fast_timing().silence_limit * 3).await;
        let mut rtts = 0;
        while let Ok(ev) = evt_rx.try_recv() {
            match ev {
                NetEvent::Rtt(_) => rtts += 1,
                NetEvent::Closed(why) => panic!("a live server was declared gone: {why}"),
                _ => {}
            }
        }
        assert!(rtts >= 10, "pongs keep arriving: {rtts}");
        cmd_tx.send(NetCommand::Leave).unwrap();
        let ended = timeout(Duration::from_secs(60), async { (session.await, server_task.await) }).await;
        let (session_result, server_result) = ended.expect("both sides must finish after Leave");
        session_result.unwrap();
        let (pings, saw_leave) = server_result.unwrap();
        assert!(pings >= 10 && saw_leave, "pings {pings}, leave {saw_leave}");
    }

    /// If the network thread vanishes without a word, the game loop must find out (once); a
    /// normal goodbye must not be reported as a failure.
    #[test]
    fn a_vanished_network_thread_is_reported_once_unless_we_said_goodbye() {
        let make = || {
            let (cmd_tx, _cmd_rx) = mpsc::unbounded_channel();
            let (evt_tx, events) = mpsc::channel(4);
            let handle = NetHandle {
                cmd_tx,
                events,
                thread: None,
                leaving: false,
                closed_reported: false,
            };
            (handle, evt_tx)
        };
        // The sender disappears without a `Closed`: reported exactly once.
        let (mut h, tx) = make();
        assert!(h.try_next().is_none(), "nothing queued yet is not an error");
        drop(tx);
        assert!(matches!(h.try_next(), Some(NetEvent::Closed(why)) if why.contains("ended unexpectedly")));
        assert!(h.try_next().is_none(), "reported only once");

        // We said goodbye, then the thread ended: the normal case, nothing to report.
        let (mut h, tx) = make();
        h.leave_and_join(Duration::from_millis(10));
        drop(tx);
        assert!(h.try_next().is_none());

        // A real `Closed` followed by the disconnect is not reported a second time.
        let (mut h, tx) = make();
        tx.try_send(NetEvent::Closed("the server closed the connection".into())).unwrap();
        drop(tx);
        assert!(matches!(h.try_next(), Some(NetEvent::Closed(why)) if why.contains("closed the connection")));
        assert!(h.try_next().is_none());
    }

    #[test]
    fn rtt_comes_from_the_echoed_nonce_and_rejects_bogus_values() {
        let now = Duration::from_millis(5_000);
        assert_eq!(rtt_from_nonce(now, 4_990_000), Some(Duration::from_millis(10)));
        assert_eq!(rtt_from_nonce(now, 5_000_000), Some(Duration::ZERO));
        assert_eq!(rtt_from_nonce(now, 5_000_001), None, "nonce from the future");
        assert_eq!(rtt_from_nonce(now, u64::MAX), None);
        assert_eq!(rtt_from_nonce(Duration::from_secs(100), 1), None, "older than a minute");
    }
}
