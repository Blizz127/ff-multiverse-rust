//! One task per connection: handshake, in-game message loop and teardown.
//!
//! Each connection owns a bounded outbound queue that a dedicated writer task drains to the
//! socket. The reader side (`Conn`) never blocks on a slow peer: every reply is queued with
//! `try_send`, and a full queue ends the connection.
//!
//! Connection flow:
//! `Hello` -> `Welcome` + `AreaManifest` -> `JoinArea` (or `CannotJoin`) -> spawn ->
//! `AreaSnapshot` to the joiner / `PlayerJoined` to the others -> `MoveIntent`/`Ping`/`Leave`.
//!
//! Everything read from the socket is untrusted: no `unwrap`/indexing on it, every string that
//! is echoed to other players or to the log is clamped first, and frame size is bounded by
//! `fmv_protocol::MAX_FRAME`.

use std::convert::Infallible;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use fmv_core::{AnimState, AreaId, AvatarAppearance, EntityId, Fingerprint, Pos, SessionId};
use fmv_protocol::{read_msg, write_msg, ClientMsg, ProtoError, ServerMsg, MAX_FRAME, PROTOCOL_VERSION};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};
use tokio::net::tcp::OwnedReadHalf;
use tokio::net::TcpStream;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn, Instrument};

use crate::config::ServerConfig;
use crate::world::{CloseFlag, MoveOutcome, NewPlayer, Outbound, OutboundTx, World};

/// Reported in `Welcome::server_build` and the READY line.
pub const SERVER_BUILD: &str = env!("CARGO_PKG_VERSION");
/// Text of the `Reject` sent to every client when the server stops.
pub const SHUTDOWN_REASON: &str = "server shutting down";
/// Longest `appearance_ref` (in chars) the server keeps. Together with the 24-char display name
/// this bounds every entity snapshot, and therefore the size of the `AreaSnapshot` frame.
pub const MAX_APPEARANCE_REF: usize = 64;

/// Longest client-supplied string (in chars) copied into a log line.
const MAX_LOG_STR: usize = 64;
/// How long a finished connection waits for its farewell to reach the socket.
const FLUSH_GRACE: Duration = Duration::from_secs(2);
/// After a farewell, keep reading (and discarding) briefly: closing a socket that still has
/// unread inbound data makes the OS send a TCP reset, which can destroy the farewell in flight.
const LINGER: Duration = Duration::from_millis(500);
const LINGER_BUDGET: usize = 256 * 1024;
/// TCP keepalive: seconds between probes once the connection has been idle for
/// `ServerConfig::keepalive`, and how many unanswered probes end it.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);
const KEEPALIVE_RETRIES: u32 = 3;

/// State shared by the accept loop, the tick loop and every connection.
pub(crate) struct Shared {
    pub cfg: ServerConfig,
    pub world: Mutex<World>,
    /// Prebuilt `AreaManifest` (path-free provenance summary only).
    pub manifest: Outbound,
    pub area_id: AreaId,
    pub fingerprint: Fingerprint,
}

impl Shared {
    /// Lock the world. The lock is only ever held for short in-memory sections and never across
    /// an `.await`. A poisoned lock is recovered rather than cascading panics into every
    /// connection.
    pub fn world(&self) -> MutexGuard<'_, World> {
        self.world.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Resolves once the flag is `true` or its sender is gone. Cancel safe.
pub(crate) async fn wait_flag(rx: &mut watch::Receiver<bool>) {
    loop {
        if *rx.borrow_and_update() {
            return;
        }
        if rx.changed().await.is_err() {
            return;
        }
    }
}

/// Token bucket: `per_sec` messages per second sustained, with one second's worth of burst.
pub(crate) struct RateLimiter {
    rate: f64,
    capacity: f64,
    tokens: f64,
    last: Instant,
}

impl RateLimiter {
    pub fn new(per_sec: u32, now: Instant) -> Self {
        let rate = f64::from(per_sec);
        Self { rate, capacity: rate, tokens: rate, last: now }
    }

    pub fn allow(&mut self, now: Instant) -> bool {
        let dt = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + dt * self.rate).min(self.capacity);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// Why a connection ended, and what (if anything) the client is told.
#[derive(Debug)]
enum Close {
    /// The peer closed the socket or it failed; nothing more can be sent.
    PeerGone,
    /// The client ended the session itself (`Leave` / `CannotJoin`).
    Left,
    /// The server closes the connection and tells the client why.
    Reject(String),
    /// The server is stopping.
    Shutdown,
    /// The world or the writer already dropped this connection (slow client, write failure).
    Dropped(&'static str),
}

impl Close {
    fn farewell(&self) -> Option<String> {
        match self {
            Close::Reject(reason) => Some(reason.clone()),
            Close::Shutdown => Some(SHUTDOWN_REASON.to_string()),
            Close::PeerGone | Close::Left | Close::Dropped(_) => None,
        }
    }
}

/// Removes the player from the world when dropped, so no code path (including a panic that
/// unwinds the task) can leave a ghost entity behind.
struct JoinGuard {
    shared: Arc<Shared>,
    entity: EntityId,
    session: SessionId,
}

impl Drop for JoinGuard {
    fn drop(&mut self) {
        let (left, online) = {
            let mut world = self.shared.world();
            (world.leave(self.entity), world.player_count())
        };
        if left {
            info!(entity = self.entity.0, session = self.session.0, online, "player left");
        }
    }
}

/// Validate the fields of `Hello` and build the avatar. `Err` carries the `Reject` reason.
pub(crate) fn check_hello(
    protocol_version: u16,
    display_name: &str,
    tint: [u8; 3],
    appearance_ref: Option<String>,
) -> Result<AvatarAppearance, String> {
    if protocol_version != PROTOCOL_VERSION {
        return Err(format!(
            "protocol version mismatch: server speaks v{PROTOCOL_VERSION}, client sent v{protocol_version}"
        ));
    }
    let Some(display_name) = AvatarAppearance::sanitize_name(display_name) else {
        return Err("display name must not be empty".to_string());
    };
    Ok(AvatarAppearance { display_name, tint, appearance_ref: clean_appearance_ref(appearance_ref) })
}

/// The appearance ref is broadcast to every player, so it is bounded and stripped of control
/// characters exactly like the display name.
fn clean_appearance_ref(raw: Option<String>) -> Option<String> {
    let cleaned: String = raw?.chars().filter(|c| !c.is_control()).take(MAX_APPEARANCE_REF).collect();
    let cleaned = cleaned.trim();
    (!cleaned.is_empty()).then(|| cleaned.to_string())
}

/// Clamp untrusted text before it goes into a log line.
fn clip_for_log(raw: &str) -> String {
    raw.chars().filter(|c| !c.is_control()).take(MAX_LOG_STR).collect()
}

fn check_join(shared: &Shared, area: &AreaId, fingerprint: &Fingerprint) -> Result<(), String> {
    // Never echo client-supplied strings back: they are unbounded.
    if *area != shared.area_id {
        return Err(format!("area mismatch: this server hosts {}", shared.area_id));
    }
    if *fingerprint != shared.fingerprint {
        return Err(format!(
            "fingerprint mismatch for area {}: your owned copy differs from the server's (different edition, region or revision?); client sent {:?}, server has {:?}",
            shared.area_id, fingerprint, shared.fingerprint
        ));
    }
    Ok(())
}

/// Read and discard inbound data until EOF, a short timeout or a byte budget.
pub(crate) async fn linger<R: AsyncRead + Unpin>(rd: &mut R) {
    let mut buf = [0u8; 1024];
    let mut budget = LINGER_BUDGET;
    let drain = async {
        while budget > 0 {
            match rd.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => budget = budget.saturating_sub(n),
            }
        }
    };
    let _ = tokio::time::timeout(LINGER, drain).await;
}

/// Drains the outbound queue to the socket. Ends when the queue is closed and empty, on a write
/// error, or when one frame cannot be written within `write_timeout`. Always raises the close
/// flag on the way out so the reader side notices a dead writer.
///
/// A peer that vanished mid-write is routine (`debug`). The other ways to end are not, and an
/// operator needs the cause at the default log level: a client that stopped reading
/// (`write_timeout`) and a message the protocol cannot carry (`ProtoError::FrameTooLarge`, for
/// example a full-server `AreaSnapshot` outgrowing `MAX_FRAME` after a protocol change, which
/// would otherwise show up only as an unexplained "dropping connection").
async fn writer_task<W: AsyncWrite + Unpin>(mut wr: W, mut rx: mpsc::Receiver<Outbound>, write_timeout: Duration, close: CloseFlag) {
    while let Some(msg) = rx.recv().await {
        match tokio::time::timeout(write_timeout, write_msg(&mut wr, &*msg)).await {
            Ok(Ok(())) => {}
            Ok(Err(ProtoError::Io(e))) => {
                debug!(error = %e, "write failed; closing connection");
                break;
            }
            Ok(Err(e)) => {
                warn!(error = %e, "cannot send a server message; closing connection");
                break;
            }
            Err(_) => {
                warn!(timeout = ?write_timeout, "client is not reading (write timed out); closing connection");
                break;
            }
        }
    }
    close.send_replace(true);
}

/// Socket options for an accepted connection. Failures are not fatal (the connection still
/// works, only without the option).
///
/// `TCP_NODELAY`: moves are small and latency-sensitive. TCP keepalive: a peer that disappears
/// without a FIN/RST (power loss, suspended laptop, NAT or Wi-Fi drop) would otherwise leave its
/// socket, slot and avatar behind for as long as the world is quiet, because the server never
/// writes to it. `idle_timeout` is the main defence; keepalive also covers `idle_timeout` being
/// disabled.
pub(crate) fn configure_socket(stream: &TcpStream, keepalive: Option<Duration>) {
    if let Err(e) = stream.set_nodelay(true) {
        debug!(error = %e, "could not set TCP_NODELAY");
    }
    if let Some(idle) = keepalive {
        let params = socket2::TcpKeepalive::new()
            .with_time(idle)
            .with_interval(KEEPALIVE_INTERVAL)
            .with_retries(KEEPALIVE_RETRIES);
        if let Err(e) = socket2::SockRef::from(stream).set_tcp_keepalive(&params) {
            debug!(error = %e, "could not enable TCP keepalive");
        }
    }
}

struct Conn {
    shared: Arc<Shared>,
    rd: OwnedReadHalf,
    tx: OutboundTx,
    close: CloseFlag,
    close_rx: watch::Receiver<bool>,
    shutdown: watch::Receiver<bool>,
    limiter: RateLimiter,
    guard: Option<JoinGuard>,
    rejected_moves: u64,
}

/// Run one accepted connection to completion.
pub(crate) async fn serve_connection(shared: Arc<Shared>, stream: TcpStream, shutdown: watch::Receiver<bool>) {
    configure_socket(&stream, shared.cfg.keepalive);
    let (rd, wr) = stream.into_split();
    let (tx, rx) = mpsc::channel::<Outbound>(shared.cfg.outbound_queue);
    let close: CloseFlag = Arc::new(watch::channel(false).0);
    let close_rx = close.subscribe();
    // `in_current_span`: the writer's log lines carry the connection's peer address too.
    let writer = tokio::spawn(
        writer_task(wr, rx, shared.cfg.write_timeout, Arc::clone(&close)).in_current_span(),
    );
    let limiter = RateLimiter::new(shared.cfg.max_msgs_per_sec, Instant::now());
    let mut conn = Conn { shared, rd, tx, close, close_rx, shutdown, limiter, guard: None, rejected_moves: 0 };
    let reason = match conn.drive().await {
        Err(reason) => reason,
        Ok(never) => match never {},
    };
    conn.finish(reason, writer).await;
}

impl Conn {
    async fn drive(&mut self) -> Result<Infallible, Close> {
        let shared = Arc::clone(&self.shared);

        // 1. Hello
        let hello = self.recv(Some(shared.cfg.hello_timeout), "timed out waiting for Hello").await?;
        let ClientMsg::Hello { protocol_version, client_build, display_name, tint, appearance_ref } = hello else {
            return Err(Close::Reject("expected Hello as the first message".to_string()));
        };
        let avatar = check_hello(protocol_version, &display_name, tint, appearance_ref).map_err(Close::Reject)?;
        debug!(client_build = %clip_for_log(&client_build), name = ?avatar.display_name, "hello accepted");

        // 2. Welcome + the area on offer
        let (session, entity) = shared.world().allocate_ids();
        self.send(ServerMsg::Welcome {
            protocol_version: PROTOCOL_VERSION,
            server_build: SERVER_BUILD.to_string(),
            session,
            entity,
        })?;
        self.send_shared(Arc::clone(&shared.manifest))?;

        // 3. The client confirms it holds exactly this area locally, or declines.
        // One total deadline for the whole step, so trickling Pings cannot hold the slot open.
        let join_started = Instant::now();
        loop {
            let remaining = shared.cfg.join_timeout.saturating_sub(join_started.elapsed());
            match self.recv(Some(remaining), "timed out waiting for JoinArea").await? {
                ClientMsg::JoinArea { area, fingerprint } => {
                    check_join(&shared, &area, &fingerprint).map_err(Close::Reject)?;
                    break;
                }
                ClientMsg::CannotJoin { area, reason } => {
                    info!(
                        area = %clip_for_log(&area.key),
                        reason = %clip_for_log(&reason),
                        "client cannot join the offered area; closing"
                    );
                    return Err(Close::Left);
                }
                ClientMsg::Leave => return Err(Close::Left),
                ClientMsg::Ping { nonce } => self.pong(nonce)?,
                ClientMsg::Hello { .. } | ClientMsg::MoveIntent { .. } => {
                    return Err(Close::Reject("unexpected message before JoinArea".to_string()));
                }
            }
        }

        // 4. Spawn. The guard is created first so that every exit path removes the entity.
        self.guard = Some(JoinGuard { shared: Arc::clone(&shared), entity, session });
        let name = avatar.display_name.clone();
        let (spawn, online) = {
            let mut world = shared.world();
            let new = NewPlayer { session, entity, avatar, tx: self.tx.clone(), close: Arc::clone(&self.close) };
            (world.join(new, Instant::now()), world.player_count())
        };
        info!(
            entity = entity.0,
            session = session.0,
            name = ?name,
            spawn_x = spawn.x,
            spawn_y = spawn.y,
            online,
            "player joined"
        );

        // 5. In-game
        loop {
            match self.recv(shared.cfg.idle_timeout, "idle timeout (no message received; send a Ping every second or so to stay connected)").await? {
                ClientMsg::MoveIntent { seq, pos, facing, anim } => self.on_move(entity, seq, pos, facing, anim)?,
                ClientMsg::Ping { nonce } => self.pong(nonce)?,
                ClientMsg::Leave => return Err(Close::Left),
                ClientMsg::Hello { .. } | ClientMsg::JoinArea { .. } | ClientMsg::CannotJoin { .. } => {
                    return Err(Close::Reject("unexpected message after joining".to_string()));
                }
            }
        }
    }

    /// Read the next client message, honouring shutdown, the close flag, an optional timeout and
    /// the per-connection rate limit. The read is dropped mid-frame only when the connection is
    /// being torn down anyway, so losing partial data is harmless.
    async fn recv(&mut self, limit: Option<Duration>, on_timeout: &'static str) -> Result<ClientMsg, Close> {
        let rd = &mut self.rd;
        let shutdown = &mut self.shutdown;
        let close_rx = &mut self.close_rx;
        let read = async {
            let fut = read_msg::<_, ClientMsg>(rd);
            match limit {
                Some(d) => tokio::time::timeout(d, fut).await.ok(),
                None => Some(fut.await),
            }
        };
        let outcome = tokio::select! {
            biased;
            () = wait_flag(shutdown) => return Err(Close::Shutdown),
            () = wait_flag(close_rx) => return Err(Close::Dropped("closed by the world or the writer")),
            r = read => r,
        };
        let msg = match outcome {
            None => return Err(Close::Reject(on_timeout.to_string())),
            Some(Ok(msg)) => msg,
            Some(Err(ProtoError::Closed | ProtoError::Io(_))) => return Err(Close::PeerGone),
            Some(Err(ProtoError::FrameTooLarge(n))) => {
                return Err(Close::Reject(format!("frame too large ({n} bytes, maximum {MAX_FRAME})")));
            }
            Some(Err(ProtoError::Codec(_))) => return Err(Close::Reject("malformed message".to_string())),
        };
        if !self.limiter.allow(Instant::now()) {
            return Err(Close::Reject("rate limit exceeded".to_string()));
        }
        Ok(msg)
    }

    fn send(&self, msg: ServerMsg) -> Result<(), Close> {
        self.send_shared(Arc::new(msg))
    }

    fn send_shared(&self, msg: Outbound) -> Result<(), Close> {
        match self.tx.try_send(msg) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(Close::Dropped("outbound queue full (slow client)")),
            Err(TrySendError::Closed(_)) => Err(Close::Dropped("writer closed")),
        }
    }

    fn pong(&self, nonce: u64) -> Result<(), Close> {
        let tick = self.shared.world().current_tick();
        self.send(ServerMsg::Pong { nonce, tick })
    }

    fn on_move(&mut self, entity: EntityId, seq: u32, pos: Pos, facing: f32, anim: AnimState) -> Result<(), Close> {
        let outcome = self.shared.world().apply_move(entity, pos, facing, anim, Instant::now());
        match outcome {
            MoveOutcome::Applied => Ok(()),
            MoveOutcome::Rejected { corrected, reason } => {
                self.rejected_moves += 1;
                if self.rejected_moves == 1 {
                    info!(entity = entity.0, ?reason, "move rejected (further rejections on this connection are logged at debug)");
                } else {
                    debug!(entity = entity.0, ?reason, "move rejected");
                }
                self.send(ServerMsg::MoveRejected { seq, corrected })
            }
            MoveOutcome::Gone => Err(Close::Dropped("removed from the world")),
        }
    }

    /// Tell the client why (if applicable), leave the world, flush and close.
    async fn finish(mut self, reason: Close, mut writer: JoinHandle<()>) {
        match &reason {
            Close::PeerGone => debug!("peer disconnected"),
            Close::Left => debug!("client left"),
            Close::Reject(why) => info!(reason = %why, "closing connection"),
            Close::Shutdown => debug!("closing connection for shutdown"),
            Close::Dropped(why) => warn!(why, "dropping connection"),
        }
        let farewell = reason.farewell();
        if let Some(text) = &farewell {
            // Best effort: if the queue is full this client is not going to read it anyway.
            let _ = self.tx.try_send(Arc::new(ServerMsg::Reject { reason: text.clone() }));
        }
        // Leaving the world drops the world's queue handle; dropping ours then closes the queue,
        // which lets the writer drain what is left and exit.
        drop(self.guard.take());
        drop(self.tx);
        // A dropped connection's writer is not worth waiting for. Otherwise give what is queued
        // (the farewell, if any) a moment to reach the socket.
        if matches!(reason, Close::Dropped(_)) || tokio::time::timeout(FLUSH_GRACE, &mut writer).await.is_err() {
            writer.abort();
        }
        if farewell.is_some() {
            linger(&mut self.rd).await;
        }
    }
}

/// Used by the accept loop when the connection cap is reached.
pub(crate) async fn reject_and_close(mut stream: TcpStream, reason: &str, write_timeout: Duration) {
    let msg = ServerMsg::Reject { reason: reason.to_string() };
    let _ = tokio::time::timeout(write_timeout, write_msg(&mut stream, &msg)).await;
    let _ = tokio::io::AsyncWriteExt::shutdown(&mut stream).await;
    linger(&mut stream).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limiter_allows_a_burst_then_refills_at_the_sustained_rate() {
        let t0 = Instant::now();
        let mut l = RateLimiter::new(120, t0);
        // One second's worth of burst is allowed at once...
        assert!((0..120).all(|_| l.allow(t0)));
        // ...then the bucket is empty.
        assert!(!l.allow(t0));
        // 25 ms refills 3 tokens at 120/s.
        let t1 = t0 + Duration::from_millis(25);
        assert!(l.allow(t1) && l.allow(t1) && l.allow(t1));
        assert!(!l.allow(t1));
        // The bucket never holds more than one second's worth, however long the pause.
        let t2 = t1 + Duration::from_secs(60);
        assert!((0..120).all(|_| l.allow(t2)));
        assert!(!l.allow(t2));
    }

    #[test]
    fn a_steady_legitimate_rate_is_never_limited() {
        let t0 = Instant::now();
        let mut l = RateLimiter::new(120, t0);
        for i in 0..10_000u64 {
            // 100 msgs/s for 100 s
            assert!(l.allow(t0 + Duration::from_millis(i * 10)));
        }
    }

    #[test]
    fn hello_checks() {
        let ok = check_hello(PROTOCOL_VERSION, "  Cloud\n", [1, 2, 3], Some("cloud".into())).unwrap();
        assert_eq!(ok.display_name, "Cloud");
        assert_eq!(ok.tint, [1, 2, 3]);
        assert_eq!(ok.appearance_ref.as_deref(), Some("cloud"));

        let err = check_hello(PROTOCOL_VERSION + 1, "Cloud", [0; 3], None).unwrap_err();
        assert!(err.contains("protocol version mismatch"), "{err}");
        assert!(check_hello(PROTOCOL_VERSION, " \t\n", [0; 3], None).is_err());
        assert!(check_hello(PROTOCOL_VERSION, "\u{7}\u{0}", [0; 3], None).is_err());

        // Overlong names are clamped, overlong/dirty appearance refs are bounded and cleaned.
        let long = "x".repeat(10_000);
        let a = check_hello(PROTOCOL_VERSION, &long, [0; 3], Some(format!("\u{1b}[31m{long}"))).unwrap();
        assert_eq!(a.display_name.chars().count(), AvatarAppearance::MAX_NAME);
        let r = a.appearance_ref.unwrap();
        assert_eq!(r.chars().count(), MAX_APPEARANCE_REF);
        assert!(!r.chars().any(char::is_control));
        // A ref that is empty after cleaning means "no ref".
        assert_eq!(check_hello(PROTOCOL_VERSION, "A", [0; 3], Some(" \n".into())).unwrap().appearance_ref, None);
    }

    /// Collects what a subscriber at the default level (`info`) would print.
    #[derive(Clone, Default)]
    struct LogBuf(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for LogBuf {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuf {
        type Writer = LogBuf;
        fn make_writer(&'a self) -> LogBuf {
            self.clone()
        }
    }

    /// Run `writer_task` over `wr` with one queued message and report what the default log showed
    /// and whether the close flag was raised. (Current-thread runtime: the thread-local
    /// subscriber sees every event.)
    async fn writer_run<W: AsyncWrite + Unpin>(wr: W, msg: ServerMsg, write_timeout: Duration) -> (String, bool) {
        let logs = LogBuf::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_max_level(tracing::Level::INFO)
            .with_ansi(false)
            .finish();
        let _default = tracing::subscriber::set_default(subscriber);
        let (tx, rx) = mpsc::channel::<Outbound>(4);
        tx.send(Arc::new(msg)).await.unwrap();
        drop(tx); // a successful write then ends the task instead of waiting for more
        let close: CloseFlag = Arc::new(watch::channel(false).0);
        let close_rx = close.subscribe();
        writer_task(wr, rx, write_timeout, close).await;
        let raised = *close_rx.borrow();
        let text = String::from_utf8_lossy(&logs.0.lock().unwrap()).into_owned();
        (text, raised)
    }

    #[tokio::test]
    async fn write_side_failures_are_logged_with_their_cause_at_the_default_level() {
        let hello = || ServerMsg::Reject { reason: "a message longer than the 16 byte pipe".into() };

        // A message the protocol cannot carry (for example a full server's snapshot outgrowing
        // MAX_FRAME after a protocol change) must say so, not just "dropping connection".
        let too_big = ServerMsg::Reject { reason: "x".repeat(MAX_FRAME as usize) };
        let (logs, closed) = writer_run(tokio::io::sink(), too_big, Duration::from_secs(5)).await;
        assert!(closed);
        assert!(logs.contains("WARN") && logs.contains("frame too large"), "{logs}");

        // A client that stopped reading: its pipe fills up and the write times out.
        let (_reader_kept_open, pipe) = tokio::io::duplex(16);
        let (logs, closed) = writer_run(pipe, hello(), Duration::from_millis(50)).await;
        assert!(closed);
        assert!(logs.contains("WARN") && logs.contains("not reading"), "{logs}");

        // A peer that is simply gone is routine and stays out of the default log...
        let (reader, pipe) = tokio::io::duplex(16);
        drop(reader);
        let (logs, closed) = writer_run(pipe, hello(), Duration::from_secs(5)).await;
        assert!(closed);
        assert!(logs.is_empty(), "{logs}");

        // ...and so does an ordinary close after everything was delivered.
        let (logs, closed) = writer_run(tokio::io::sink(), ServerMsg::Pong { nonce: 1, tick: 2 }, Duration::from_secs(5)).await;
        assert!(closed);
        assert!(logs.is_empty(), "{logs}");
    }

    #[tokio::test]
    async fn accepted_sockets_get_nodelay_and_keepalive_timers() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (client, accepted) = tokio::join!(TcpStream::connect(addr), listener.accept());
        let _client = client.unwrap();
        let (stream, _) = accepted.unwrap();

        configure_socket(&stream, Some(Duration::from_secs(30)));
        let sock = socket2::SockRef::from(&stream);
        assert!(stream.nodelay().unwrap());
        assert!(sock.keepalive().unwrap(), "SO_KEEPALIVE must be on");
        assert_eq!(sock.tcp_keepalive_time().unwrap(), Duration::from_secs(30));
        assert_eq!(sock.tcp_keepalive_interval().unwrap(), KEEPALIVE_INTERVAL);
        assert_eq!(sock.tcp_keepalive_retries().unwrap(), KEEPALIVE_RETRIES);

        // Disabled in the config: left alone.
        let (client, accepted) = tokio::join!(TcpStream::connect(addr), listener.accept());
        let _client = client.unwrap();
        let (plain, _) = accepted.unwrap();
        configure_socket(&plain, None);
        assert!(!socket2::SockRef::from(&plain).keepalive().unwrap());
    }

    #[test]
    fn farewells() {
        assert_eq!(Close::Shutdown.farewell().as_deref(), Some("server shutting down"));
        assert_eq!(Close::Reject("x".into()).farewell().as_deref(), Some("x"));
        assert!(Close::PeerGone.farewell().is_none());
        assert!(Close::Left.farewell().is_none());
        assert!(Close::Dropped("slow").farewell().is_none());
    }

    #[test]
    fn log_clipping_strips_control_chars_and_bounds_length() {
        let s = clip_for_log(&format!("a\nb\r\u{1b}[0m{}", "z".repeat(500)));
        assert!(!s.contains('\n') && !s.contains('\u{1b}'));
        assert_eq!(s.chars().count(), MAX_LOG_STR);
    }
}
