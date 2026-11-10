//! One task per connection: handshake, in-game message loop and teardown.
//!
//! Each connection owns a bounded outbound queue that a dedicated writer task drains to the
//! socket. The reader side (`Conn`) never blocks on a slow peer: every reply is queued with
//! `try_send`, and a full queue ends the connection.
//!
//! Connection flow:
//! `Hello` (token or registration, checked against the account store) -> `Welcome` +
//! `AreaManifest` -> `JoinArea` (or `CannotJoin`; the area, fingerprint and edition are checked)
//! -> spawn -> `AreaSnapshot` and `InstancePlacement` to the joiner / `PlayerJoined` to the
//! others -> `MoveIntent` / `Ping` / party requests / `Leave`. Walking through an exit makes the
//! world offer the player another area (`AreaChange`); it answers `JoinArea` or `CannotJoin`
//! exactly as at the first join.
//!
//! Everything read from the socket is untrusted: no `unwrap`/indexing on it, every string that
//! is echoed to other players or to the log is clamped first, and frame size is bounded by
//! `fmv_protocol::MAX_FRAME`.
//!
//! Every refusal is `Reject { reason, code }` ([`RejectCode`]): the text is for people, the code
//! is what a client acts on.
//!
//! # Registration and the token
//!
//! A registration is the one request whose result cannot be asked for again: the account exists
//! the moment it is committed, its name is taken, and the only copy of its token is the `Welcome`.
//! So (see also `accounts`):
//!
//! * everything the client chose is validated, and the registration rate limits are consulted,
//!   BEFORE anything is created, and a limit slot is only used up by an account that really was
//!   created;
//! * just before the commit the connection is probed ([`peer_still_connected`]): a peer that has
//!   already hung up or reset gets no account;
//! * the connection remembers which account it just registered until the writer reports that the
//!   `Welcome` was written to the socket; if the connection ends first, the account is released
//!   (`accounts::release_unclaimed`) and its name is free again;
//! * once the client answers (the first message that decodes after the `Welcome`, whatever it
//!   asks for), the account is recorded as claimed (`accounts::claim_delivered`): the `Welcome` was
//!   written and answered, so the store's orphan tools (`release_unclaimed`, `fmv-accounts
//!   release-older-than`) can from then on only ever free an account whose token never got that far.
//!   (A client that pipelines a second message behind its `Hello` without reading the `Welcome` is
//!   claimed too: that costs nothing beyond the name squatting open registration already allows.)
//!
//! # Idle sessions
//!
//! Two clocks end a joined session: `idle_timeout` (it sent NOTHING, a vanished peer) and
//! `inactivity_timeout` (it sent only `Ping`s and no game input, a squatter). See
//! `ServerConfig::inactivity_timeout`.

use std::convert::Infallible;
use std::mem::MaybeUninit;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fmv_core::{AccountId, AnimState, AreaId, EntityId, Fingerprint, Pos, ProvenanceSummary, SessionId};
use fmv_persist::Store;
use fmv_protocol::{
    decode, probe_hello_version, read_frame, write_msg, Auth, ClientMsg, NewCharacter, ProtoError, RejectCode, Secret, ServerMsg,
    MAX_FRAME, PROTOCOL_VERSION,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};
use tokio::net::tcp::OwnedReadHalf;
use tokio::net::TcpStream;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn, Instrument};

use crate::accounts::{self, Login, Release};
use crate::config::ServerConfig;
use crate::deferred;
use crate::deletion::{Begin, Tombstones, DELETE_BUSY, DELETE_ONLINE};
use crate::limits::{throttle_notice, Limits, LogVerdict, PeerKey, RegistrationRefusal};
use crate::world::{
    BattleReply, CloseFlag, JoinError, MoveOutcome, NewPlayer, Outbound, OutboundTx, PartyOutcome, PressOutcome, SceneReply, TravelReply, World,
};

/// Reported in `Welcome::server_build` and the READY line.
pub const SERVER_BUILD: &str = env!("CARGO_PKG_VERSION");
/// Text of the `Reject` sent to every client when the server stops.
pub const SHUTDOWN_REASON: &str = "server shutting down";
/// Text of the `Reject` for a second session of an account that is already in the world.
const ALREADY_ONLINE: &str = "this account is already logged in";
/// Text of the `Reject` (`InvalidToken`) for a token the server does not know, and for a login of an account that was
/// deleted after its token was checked. It never says which: that is what a client reads as "this login is gone".
const UNKNOWN_TOKEN: &str = "invalid or unknown token";
/// Text of the `Reject` for a registration on a server that takes no new accounts.
const REGISTRATION_CLOSED: &str = "registration is closed on this server; log in with a saved token";

/// What a read that ran out of time becomes: the code and the text of the farewell.
type OnTimeout = (RejectCode, &'static str);
const HELLO_TIMEOUT: OnTimeout = (RejectCode::Timeout, "timed out waiting for Hello");
const JOIN_TIMEOUT: OnTimeout = (RejectCode::Timeout, "timed out waiting for JoinArea");
const SILENCE_TIMEOUT: OnTimeout =
    (RejectCode::IdleTimeout, "idle timeout (no message received; send a Ping every second or so to stay connected)");

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
    /// The world. Reach it only through [`Shared::with_world`]: that is what keeps log lines
    /// out of the critical section.
    pub world: Mutex<World>,
    /// Prebuilt `AreaManifest` (path-free provenance summary only).
    pub manifest: Outbound,
    pub area_id: AreaId,
    pub fingerprint: Fingerprint,
    /// The hosted area's path-free provenance: a joining client's must be `compatible_with` it.
    pub provenance: ProvenanceSummary,
    /// `Welcome::server_build` and the READY line's `build=`: the crate version, plus the
    /// adapter's identity when the configuration names one (see `ServerConfig::build_suffix`).
    pub build: String,
    /// Accounts, avatar profiles and progression. Blocking: use it under `spawn_blocking`.
    pub store: Arc<Store>,
    /// Saves what battles pay to the store (`None`: this server runs no battles). Stopped, and waited
    /// for, when the server shuts down (see `Server::serve`).
    pub saver: Option<crate::saver::Saver>,
    /// Per-address connection cap, registration rate limits and the log throttle.
    pub limits: Limits,
    /// The accounts deleted (or being deleted) in this run. Used only inside the world lock: see `deletion`.
    pub deleted: Tombstones,
    /// Keeps an ephemeral state directory alive. Declared after `store` so the store (and its
    /// lock file) is dropped before the directory is removed.
    pub _ephemeral_state: Option<tempfile::TempDir>,
}

impl Shared {
    /// Run `f` on the locked world. The lock is only ever held for short in-memory sections and
    /// never across an `.await`. A poisoned lock is recovered rather than cascading panics into
    /// every connection. What `f` logged is written AFTER the lock is released (see `deferred`),
    /// so a stalled log sink cannot stall everyone who needs the world.
    pub fn with_world<R>(&self, f: impl FnOnce(&mut World) -> R) -> R {
        deferred::with_locked(&self.world, f)
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
    Reject(RejectCode, String),
    /// The server is stopping.
    Shutdown,
    /// The world or the writer already dropped this connection (slow client, write failure).
    Dropped(&'static str),
    /// The server answered a request that is not a session (a delete) and closes the connection. The answer is
    /// already queued: it is flushed, and the socket is read for a moment like after a farewell, so that a reset
    /// cannot destroy it in flight.
    Answered,
}

impl Close {
    fn reject(code: RejectCode, text: impl Into<String>) -> Self {
        Close::Reject(code, text.into())
    }

    fn farewell(&self) -> Option<(RejectCode, String)> {
        match self {
            Close::Reject(code, reason) => Some((*code, reason.clone())),
            Close::Shutdown => Some((RejectCode::ShuttingDown, SHUTDOWN_REASON.to_string())),
            Close::PeerGone | Close::Left | Close::Dropped(_) | Close::Answered => None,
        }
    }

    /// Does the connection keep reading (and discarding) for a moment after the last message is flushed?
    fn lingers(&self) -> bool {
        matches!(self, Close::Answered) || self.farewell().is_some()
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
        let (left, online) = self.shared.with_world(|world| (world.leave(self.entity), world.player_count()));
        if left {
            info!(entity = self.entity.0, session = self.session.0, online, "player left");
        }
    }
}

/// A `Hello` of another protocol version is refused with a clear reason (`Err` carries it).
pub(crate) fn check_version(protocol_version: u16) -> Result<(), String> {
    if protocol_version != PROTOCOL_VERSION {
        return Err(format!(
            "protocol version mismatch: server speaks v{PROTOCOL_VERSION}, client sent v{protocol_version}"
        ));
    }
    Ok(())
}

/// Seconds since the unix epoch (the store timestamps accounts with it).
fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Clamp untrusted text before it goes into a log line.
fn clip_for_log(raw: &str) -> String {
    raw.chars().filter(|c| !c.is_control()).take(MAX_LOG_STR).collect()
}

/// The fingerprint is the content check; the provenance the client names for its own copy must
/// ALSO be compatible with the area's (same game, platform, edition, region and revision). The
/// fingerprint of a real adapter binds the provenance already, but an adapter's fingerprint rule
/// is not something the server can assume, and a client may lie about either.
fn check_join(
    shared: &Shared,
    area: &AreaId,
    fingerprint: &Fingerprint,
    provenance: &ProvenanceSummary,
) -> Result<(), String> {
    // Never echo client-supplied strings back unbounded.
    if *area != shared.area_id {
        return Err(format!("area mismatch: this server hosts {}", shared.area_id));
    }
    if *fingerprint != shared.fingerprint {
        return Err(format!(
            "fingerprint mismatch for area {}: your owned copy differs from the server's (different edition, region or revision?); client sent {:?}, server has {:?}",
            shared.area_id, fingerprint, shared.fingerprint
        ));
    }
    if !shared.provenance.compatible_with(provenance) {
        let server = &shared.provenance;
        return Err(format!(
            "edition mismatch for area {}: the server's copy is {:?}/{:?}/{:?} ({:?} {:?}), yours is {:?}/{:?}/{:?} ({:?} {:?})",
            shared.area_id,
            server.edition,
            server.region,
            server.revision,
            server.game,
            server.platform,
            clip_for_log(&provenance.edition),
            clip_for_log(&provenance.region),
            clip_for_log(&provenance.revision),
            provenance.game,
            provenance.platform,
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

/// Is the peer still there to be answered? `false` when it has closed its end or the connection
/// was reset since its last message: nothing sent to it would ever be read.
///
/// A non-consuming, non-blocking look at the socket (`recv(MSG_PEEK)`): no data yet is the normal
/// case and counts as alive; data is alive; an orderly close (0 bytes) and every error except
/// "would block" and "interrupted" are not. A client that half-closes right after its `Hello` is
/// reported as gone too, which is right: it cannot send the `JoinArea` the handshake needs after
/// the `Welcome`, so nothing would come of the account.
fn peer_still_connected(stream: &TcpStream) -> bool {
    let mut probe = [MaybeUninit::<u8>::uninit(); 1];
    match socket2::SockRef::from(stream).peek(&mut probe) {
        Ok(0) => false,
        Ok(_) => true,
        Err(e) => matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted),
    }
}

/// Drains the outbound queue to the socket. Ends when the queue is closed and empty, on a write
/// error, or when one frame cannot be written within `write_timeout`. Always raises the close
/// flag on the way out so the reader side notices a dead writer. `written` counts the frames that
/// were completely written (the first one a registering connection sends is its `Welcome`).
///
/// A peer that vanished mid-write is routine (`debug`). The other ways to end are not, and an
/// operator needs the cause at the default log level: a client that stopped reading
/// (`write_timeout`) and a message the protocol cannot carry (`ProtoError::FrameTooLarge`, for
/// example a full-server `AreaSnapshot` outgrowing `MAX_FRAME` after a protocol change, which
/// would otherwise show up only as an unexplained "dropping connection").
async fn writer_task<W: AsyncWrite + Unpin>(
    mut wr: W,
    mut rx: mpsc::Receiver<Outbound>,
    write_timeout: Duration,
    close: CloseFlag,
    written: Arc<AtomicU64>,
) {
    while let Some(msg) = rx.recv().await {
        match tokio::time::timeout(write_timeout, write_msg(&mut wr, &*msg)).await {
            Ok(Ok(())) => {
                written.fetch_add(1, Ordering::Release);
            }
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

/// What a joined session's next read may wait for (see [`next_wait`]).
#[derive(Debug, PartialEq, Eq)]
enum Wait {
    /// Read for at most this long (`None`: no limit) and then fail with this farewell.
    Read(Option<Duration>, OnTimeout),
    /// The session has had no game input for the whole inactivity timeout.
    Inactive,
}

/// The deadline of the next read of a joined session, from its two clocks: `idle` bounds the time
/// between any two messages (silence), `inactivity` the time since the last message that was not
/// a `Ping` (`since_input`). The sooner one wins, and says which of them it was.
fn next_wait(idle: Option<Duration>, inactivity: Option<Duration>, since_input: Duration) -> Wait {
    let Some(inactivity) = inactivity else {
        return Wait::Read(idle, SILENCE_TIMEOUT);
    };
    let Some(left) = inactivity.checked_sub(since_input).filter(|left| !left.is_zero()) else {
        return Wait::Inactive;
    };
    match idle {
        Some(idle) if idle <= left => Wait::Read(Some(idle), SILENCE_TIMEOUT),
        _ => Wait::Read(
            Some(left),
            (RejectCode::IdleTimeout, "closed for inactivity (no game input; pings do not count); log in again to continue"),
        ),
    }
}

/// The farewell of a session that did nothing but ping for `limit`.
fn inactivity_farewell(limit: Duration) -> Close {
    let minutes = limit.as_secs() / 60;
    let span = if minutes >= 1 { format!("{minutes} minute(s)") } else { format!("{} second(s)", limit.as_secs()) };
    Close::reject(
        RejectCode::IdleTimeout,
        format!("closed for inactivity (no game input for {span}; pings do not count); log in again to continue"),
    )
}

/// An account this connection registered whose `Welcome` was not written: the account to release.
/// `frames_written == 0` means not even the first frame (the `Welcome`, always queued first) got
/// to the socket.
fn undelivered(registered: Option<AccountId>, frames_written: u64) -> Option<AccountId> {
    registered.filter(|_| frames_written == 0)
}

struct Conn {
    shared: Arc<Shared>,
    peer: PeerKey,
    rd: OwnedReadHalf,
    tx: OutboundTx,
    close: CloseFlag,
    close_rx: watch::Receiver<bool>,
    shutdown: watch::Receiver<bool>,
    limiter: RateLimiter,
    guard: Option<JoinGuard>,
    rejected_moves: u64,
    /// The `seq` of the newest `MoveIntent` this connection has received (what a relocation by a press is answered with).
    last_move_seq: u32,
    /// Nothing has been read yet (the first message is where a protocol mismatch is diagnosed).
    first_message: bool,
    /// Frames the writer has completely written to the socket.
    written: Arc<AtomicU64>,
    /// The account this connection registered, until the end of the connection shows that its
    /// `Welcome` was written (see the module docs).
    registered: Option<AccountId>,
}

/// Run one accepted connection to completion.
pub(crate) async fn serve_connection(shared: Arc<Shared>, stream: TcpStream, peer: SocketAddr, shutdown: watch::Receiver<bool>) {
    configure_socket(&stream, shared.cfg.keepalive);
    let (rd, wr) = stream.into_split();
    let (tx, rx) = mpsc::channel::<Outbound>(shared.cfg.outbound_queue);
    let close: CloseFlag = Arc::new(watch::channel(false).0);
    let close_rx = close.subscribe();
    let written = Arc::new(AtomicU64::new(0));
    // `in_current_span`: the writer's log lines carry the connection's peer address too.
    let writer = tokio::spawn(
        writer_task(wr, rx, shared.cfg.write_timeout, Arc::clone(&close), Arc::clone(&written)).in_current_span(),
    );
    let limiter = RateLimiter::new(shared.cfg.max_msgs_per_sec, Instant::now());
    let mut conn = Conn {
        shared,
        peer: PeerKey::of(peer.ip()),
        rd,
        tx,
        close,
        close_rx,
        shutdown,
        limiter,
        guard: None,
        rejected_moves: 0,
        last_move_seq: 0,
        first_message: true,
        written,
        registered: None,
    };
    let reason = match conn.drive().await {
        Err(reason) => reason,
        Ok(never) => match never {},
    };
    conn.finish(reason, writer).await;
}

impl Conn {
    async fn drive(&mut self) -> Result<Infallible, Close> {
        let shared = Arc::clone(&self.shared);

        // 1. Hello (or a request to delete a character: that is its own short connection, see `delete_character`)
        let hello = self.recv(Some(shared.cfg.hello_timeout), HELLO_TIMEOUT).await?;
        let (protocol_version, client_build, auth, character) = match hello {
            ClientMsg::Hello { protocol_version, client_build, auth, character } => (protocol_version, client_build, auth, character),
            ClientMsg::DeleteCharacter { protocol_version, client_build, token } => {
                return self.delete_character(protocol_version, &client_build, token).await;
            }
            _ => return Err(Close::reject(RejectCode::BadRequest, "expected Hello as the first message")),
        };
        check_version(protocol_version).map_err(|why| Close::reject(RejectCode::ProtocolMismatch, why))?;

        // 2. Who is this? Registration or token login, against the persistent store.
        let scene_save_epoch = shared.with_world(|world| world.scene_save_epoch());
        let Login { account, avatar, progression, issued_token, progress, card } = self.authenticate(auth, character).await?;
        let registered = issued_token.is_some();
        if registered {
            // Until the Welcome with its token is known to be written, this account is released
            // if the connection ends (see `finish`).
            self.registered = Some(account);
        }
        debug!(
            client_build = %clip_for_log(&client_build),
            account = account.0,
            name = ?avatar.display_name,
            registered,
            slot = ?card.slot,
            migrated = card.migrated,
            "hello accepted"
        );
        // A token login that is refused early (before `Welcome`) gets a quick answer; `World::join`
        // re-checks it atomically because two handshakes can authenticate the same account at
        // once. A brand-new account cannot be online, and nothing stands between its commit and
        // its `Welcome`.
        if !registered {
            let (online, deleted) = shared.with_world(|w| (w.is_online(account), shared.deleted.contains(account)));
            if deleted {
                // Deleted since the token was checked (or being deleted right now): to this login it is a token the
                // server does not know.
                return Err(Close::reject(RejectCode::InvalidToken, UNKNOWN_TOKEN));
            }
            if online {
                return Err(Close::reject(RejectCode::AlreadyOnline, ALREADY_ONLINE));
            }
        }

        // 3. Welcome (the only message that ever carries a token) + the area on offer. The slot and the binding are kept
        // for the "player joined" line (a `key=value` line the smoke tests read).
        let (slot, migrated) = (card.slot.map_or_else(|| "-".to_string(), |s| s.to_string()), card.migrated);
        let binding = card.binding.clone().unwrap_or_else(|| "-".to_string());
        let (session, entity) = shared.with_world(World::allocate_ids);
        self.send(ServerMsg::Welcome {
            protocol_version: PROTOCOL_VERSION,
            server_build: shared.build.clone(),
            session,
            entity,
            account,
            issued_token: issued_token.map(Secret::from),
            character: card,
        })?;
        self.send_shared(Arc::clone(&shared.manifest))?;

        // 4. The client confirms it holds exactly this area locally, or declines.
        // One total deadline for the whole step, so trickling Pings cannot hold the slot open.
        let join_started = Instant::now();
        loop {
            let remaining = shared.cfg.join_timeout.saturating_sub(join_started.elapsed());
            let msg = self.recv(Some(remaining), JOIN_TIMEOUT).await?;
            // The first message a client of a new account sends after its `Welcome` answers it: as far
            // as the server can tell the token was delivered, so the account is no orphan any more.
            self.acknowledge_welcome().await;
            match msg {
                ClientMsg::JoinArea { area, fingerprint, provenance } => {
                    check_join(&shared, &area, &fingerprint, &provenance)
                        .map_err(|why| Close::reject(RejectCode::ContentMismatch, why))?;
                    break;
                }
                ClientMsg::CannotJoin { area, reason } => {
                    let (area, reason) = (clip_for_log(&area.key), clip_for_log(&reason));
                    self.budgeted(
                        |_| info!(%area, %reason, "client cannot join the offered area; closing"),
                        || debug!(%area, %reason, "client cannot join the offered area; closing (log throttled)"),
                    );
                    return Err(Close::Left);
                }
                ClientMsg::Leave => return Err(Close::Left),
                ClientMsg::Ping { nonce } => self.pong(nonce)?,
                ClientMsg::Hello { .. }
                | ClientMsg::DeleteCharacter { .. }
                | ClientMsg::MoveIntent { .. }
                | ClientMsg::Interact { .. }
                | ClientMsg::PartyInvite { .. }
                | ClientMsg::PartyAccept { .. }
                | ClientMsg::PartyDecline { .. }
                | ClientMsg::PartyLeave
                | ClientMsg::PartyKick { .. }
                | ClientMsg::PartyPromote { .. }
                | ClientMsg::PartySync
                | ClientMsg::BattleCommand { .. }
                | ClientMsg::RequestEncounter { .. }
                | ClientMsg::SceneStep { .. }
                | ClientMsg::SceneEnd { .. }
                | ClientMsg::SceneLeave { .. }
                | ClientMsg::SceneDebugPreview { .. } => {
                    return Err(Close::reject(RejectCode::BadRequest, "unexpected message before JoinArea"));
                }
            }
        }

        // 5. Spawn. The guard is created first so that every exit path removes the entity.
        self.guard = Some(JoinGuard { shared: Arc::clone(&shared), entity, session });
        let name = avatar.display_name.clone();
        let joined = {
            let new = NewPlayer { session, entity, account, avatar, progression, progress, tx: self.tx.clone(), close: Arc::clone(&self.close) };
            shared.with_world(|world| {
                // Checked here, under the world lock, against a delete that began after this login was
                // authenticated: one of the two comes first (see `deletion`).
                if shared.deleted.contains(account) {
                    return Err(Close::reject(RejectCode::InvalidToken, UNKNOWN_TOKEN));
                }
                world
                    .join_with_scene_epoch(new, Instant::now(), scene_save_epoch)
                    .map(|spawn| (spawn, world.player_count()))
                    .map_err(|error| match error {
                        JoinError::AlreadyOnline => Close::reject(RejectCode::AlreadyOnline, ALREADY_ONLINE),
                        JoinError::ProgressChanged => Close::reject(RejectCode::Internal, "A scene save completed during login. Please reconnect to load current progress."),
                        JoinError::StorageUnavailable => Close::reject(RejectCode::Internal, "Server storage requires recovery. Please reconnect after the server restarts."),
                    })
            })
        };
        let (spawn, online) = joined?;
        info!(
            entity = entity.0,
            session = session.0,
            account = account.0,
            name = ?name,
            spawn_x = spawn.x,
            spawn_y = spawn.y,
            online,
            slot = %slot,
            binding = %binding,
            migrated,
            "player joined"
        );

        // 6. In-game. A client that sends nothing is cut after `idle_timeout`; one that only pings
        // (no game input) after `inactivity_timeout`.
        let mut last_input = Instant::now();
        loop {
            let (limit, on_timeout) = match next_wait(shared.cfg.idle_timeout, shared.cfg.inactivity_timeout, last_input.elapsed()) {
                Wait::Read(limit, on_timeout) => (limit, on_timeout),
                Wait::Inactive => {
                    return Err(inactivity_farewell(shared.cfg.inactivity_timeout.unwrap_or_default()));
                }
            };
            let msg = self.recv(limit, on_timeout).await?;
            if !matches!(msg, ClientMsg::Ping { .. }) {
                last_input = Instant::now();
            }
            match msg {
                ClientMsg::MoveIntent { seq, pos, facing, anim } => self.on_move(entity, seq, pos, facing, anim)?,
                ClientMsg::Interact { npc } => self.on_interact(entity, npc)?,
                ClientMsg::Ping { nonce } => self.pong(nonce)?,
                ClientMsg::Leave => return Err(Close::Left),
                ClientMsg::PartyInvite { target } => self.party(|w, now| w.party_invite(entity, target, now))?,
                ClientMsg::PartyAccept { invite } => self.party(|w, now| w.party_accept(entity, invite, now))?,
                ClientMsg::PartyDecline { invite } => self.party(|w, _| w.party_decline(entity, invite))?,
                ClientMsg::PartyLeave => self.party(|w, _| w.party_leave(entity))?,
                ClientMsg::PartyKick { target } => self.party(|w, _| w.party_kick(entity, target))?,
                ClientMsg::PartyPromote { target } => self.party(|w, _| w.party_promote(entity, target))?,
                ClientMsg::PartySync => self.party(|w, now| w.party_sync(entity, now))?,
                ClientMsg::RequestEncounter { formation_id } => {
                    self.battle(|w, now| w.request_encounter(entity, formation_id, now))?
                }
                ClientMsg::BattleCommand { battle_id, actor, command } => {
                    self.battle(|w, now| w.battle_command(entity, battle_id, actor, command, now))?
                }
                // The answer to an `AreaChange` (a transfer between areas, see `world::areas`).
                ClientMsg::JoinArea { area, fingerprint, provenance } => {
                    self.travel(|w, now| w.join_area(entity, &area, &fingerprint, &provenance, now))?
                }
                ClientMsg::CannotJoin { area, .. } => self.travel(|w, now| w.cannot_join(entity, &area, now))?,
                // Story scenes (see `world::scene`).
                ClientMsg::SceneStep { .. } | ClientMsg::SceneEnd { .. } | ClientMsg::SceneLeave { .. } | ClientMsg::SceneDebugPreview { .. } => {
                    self.on_scene(entity, msg)?
                }
                ClientMsg::Hello { .. } | ClientMsg::DeleteCharacter { .. } => {
                    return Err(Close::reject(RejectCode::BadRequest, "unexpected message after joining"));
                }
            }
        }
    }

    /// A request to delete a character (`ClientMsg::DeleteCharacter`, the first message of its own connection): see
    /// `deletion` for the rule. Always ends the connection: with `CharacterDeleted` and [`Close::Answered`] when it was
    /// done, with a `Reject` when it was not (`InvalidToken`: the server does not know the login, so there is nothing
    /// to delete; `AlreadyOnline`: the character is in the world; `ProtocolMismatch`; `RateLimited`: a delete of the
    /// same character is under way; `Internal`: the store failed and nothing was deleted).
    async fn delete_character(&mut self, protocol_version: u16, client_build: &str, token: Secret) -> Result<Infallible, Close> {
        let shared = Arc::clone(&self.shared);
        check_version(protocol_version).map_err(|why| Close::reject(RejectCode::ProtocolMismatch, why))?;
        let store = Arc::clone(&shared.store);
        // 1. Whose character is it? The token says (and only the one who holds it can ask).
        let account = match tokio::task::spawn_blocking(move || accounts::account_of_token(&store, &token)).await {
            Ok(Ok(account)) => account,
            Ok(Err(e)) => {
                match &e {
                    accounts::LoginError::Internal(cause) => error!(%cause, "account store failure while checking the token of a delete"),
                    accounts::LoginError::Refused(code, why) => debug!(?code, %why, "delete refused: the login is not valid"),
                }
                return Err(Close::reject(e.code(), e.client_text()));
            }
            Err(e) => {
                error!(error = %e, "delete task failed");
                return Err(Close::reject(RejectCode::Internal, "internal error; try again later"));
            }
        };
        // 2. Atomic with the joins: refuse while the character is in the world, and from here on nobody joins as it.
        match shared.with_world(|world| shared.deleted.begin(account, world.is_online(account))) {
            Begin::Started => {}
            Begin::Online => return Err(Close::reject(RejectCode::AlreadyOnline, DELETE_ONLINE)),
            Begin::Busy => return Err(Close::reject(RejectCode::RateLimited, DELETE_BUSY)),
        }
        // 3. The delete itself: blocking file work, soft and atomic (see `fmv_persist::Store::delete_account`).
        let store = Arc::clone(&shared.store);
        let now = unix_now();
        let outcome = tokio::task::spawn_blocking(move || accounts::delete_character(&store, account, now)).await;
        let deletion = match outcome {
            Ok(Ok(deletion)) => deletion,
            Ok(Err(e)) => {
                // Nothing was deleted (or it was already gone): the account may join again.
                shared.deleted.cancel(account);
                match &e {
                    accounts::LoginError::Internal(cause) => error!(account = account.0, %cause, "could not delete a character"),
                    accounts::LoginError::Refused(code, why) => debug!(?code, %why, "delete refused"),
                }
                return Err(Close::reject(e.code(), e.client_text()));
            }
            Err(e) => {
                shared.deleted.cancel(account);
                error!(error = %e, "delete task failed");
                return Err(Close::reject(RejectCode::Internal, "internal error; try again later"));
            }
        };
        info!(
            client_build = %clip_for_log(client_build),
            account = account.0,
            name = ?deletion.name,
            trash_entry = %deletion.entry,
            files = deletion.files,
            unfinished = deletion.unfinished,
            "character deleted: its records are in the trash and its name is free"
        );
        self.send(ServerMsg::CharacterDeleted { name: deletion.name })?;
        Err(Close::Answered)
    }

    /// Run the answer to an `AreaChange` against the world. An answer that does not fit (no
    /// transfer is under way, the area or edition is not the one offered, a second refusal) ends the
    /// connection like any other message out of place.
    fn travel(&mut self, op: impl FnOnce(&mut World, Instant) -> TravelReply) -> Result<(), Close> {
        let reply = self.shared.with_world(|world| op(world, Instant::now()));
        match reply {
            TravelReply::Done => Ok(()),
            TravelReply::Refused(code, reason) => Err(Close::reject(code, reason)),
            TravelReply::NotTravelling => Err(Close::reject(RejectCode::BadRequest, "unexpected message after joining")),
            TravelReply::Gone => Err(Close::Dropped("removed from the world")),
        }
    }

    /// Registration or token login. The store does blocking disk I/O, so it runs off the async
    /// executor; the client is told why a refusal happened, but never anything about which
    /// tokens or names exist, and never a local path.
    ///
    /// A registration goes in this order, so that a refused or failed one costs nothing: registration
    /// open? name and look valid? a place in the rate limits (per address, then server-wide)? peer
    /// still there? and only then the commit, which uses the place up (see `limits`).
    async fn authenticate(&mut self, auth: Auth, character: NewCharacter) -> Result<Login, Close> {
        let shared = Arc::clone(&self.shared);
        let store = Arc::clone(&shared.store);
        let game = shared.area_id.game;
        let now = unix_now();
        let tint = character.tint;
        let rules = shared.cfg.battle.as_ref().map(|battle| Arc::clone(&battle.rules));
        let task = match auth {
            Auth::Token(token) => tokio::task::spawn_blocking(move || match rules.as_deref() {
                Some(rules) => accounts::login_with_token_and_rules(&store, game, &token, tint, now, rules),
                None => accounts::login_with_token(&store, game, &token, tint, now),
            }),
            Auth::Register { display_name } => {
                if !shared.cfg.allow_registration {
                    return Err(Close::reject(RejectCode::RegistrationClosed, REGISTRATION_CLOSED));
                }
                let registration = accounts::validate_registration(game, &display_name, &character).map_err(|e| {
                    debug!(code = ?e.code(), why = e.client_text(), "registration refused");
                    Close::reject(e.code(), e.client_text())
                })?;
                let permit = shared.limits.registrations.reserve(self.peer, Instant::now()).map_err(|refusal| match refusal {
                    RegistrationRefusal::Server => Close::reject(
                        RejectCode::RateLimited,
                        "too many registrations on this server right now; try again in a minute",
                    ),
                    RegistrationRefusal::Peer => Close::reject(
                        RejectCode::RateLimited,
                        "too many registrations from your address; try again later",
                    ),
                })?;
                // The last look before the commit: a peer that has already hung up or reset would
                // never read the token, and the account would be lost with it.
                if !peer_still_connected(self.rd.as_ref()) {
                    return Err(Close::PeerGone);
                }
                tokio::task::spawn_blocking(move || {
                    let on_created = move || permit.commit(Instant::now());
                    match rules.as_deref() {
                        Some(rules) => accounts::register_with_rules(&store, game, registration, now, on_created, rules),
                        None => accounts::register(&store, game, registration, now, on_created),
                    }
                })
            }
        };
        match task.await {
            Ok(Ok(login)) => Ok(login),
            Ok(Err(e)) => {
                match &e {
                    accounts::LoginError::Internal(cause) => error!(%cause, "account store failure during login"),
                    accounts::LoginError::Refused(code, why) => debug!(?code, %why, "login refused"),
                }
                Err(Close::reject(e.code(), e.client_text()))
            }
            Err(e) => {
                error!(error = %e, "login task failed");
                Err(Close::reject(RejectCode::Internal, "internal error; try again later"))
            }
        }
    }

    /// The client of an account this connection just registered has answered the `Welcome` that
    /// carried its token (any message that decodes, whatever it asks for): record that, so that the
    /// account is never mistaken for a registration whose token was lost. A registering session never
    /// logs in with its token, so nothing else would mark the account. From here on this connection
    /// no longer releases the account when it ends. Failing to record it is only logged: the client
    /// has its token and plays on (the account then looks like an orphan to the store's tools).
    async fn acknowledge_welcome(&mut self) {
        let Some(account) = self.registered.take() else { return };
        let store = Arc::clone(&self.shared.store);
        match tokio::task::spawn_blocking(move || accounts::claim_delivered(&store, account)).await {
            Ok(Ok(_)) => debug!(account = account.0, "the client answered its Welcome: the account is claimed"),
            Ok(Err(cause)) => {
                warn!(account = account.0, %cause, "could not record that a new account's token reached its client; the store will treat it as unclaimed")
            }
            Err(e) => warn!(account = account.0, error = %e, "recording that a new account's token reached its client failed"),
        }
    }

    /// Run one party request against the world and answer a refusal to this client alone.
    fn party(&mut self, op: impl FnOnce(&mut World, Instant) -> PartyOutcome) -> Result<(), Close> {
        let outcome = self.shared.with_world(|world| op(world, Instant::now()));
        match outcome {
            PartyOutcome::Done => Ok(()),
            PartyOutcome::Refused(error) => self.send(ServerMsg::PartyRejected { error }),
            PartyOutcome::Gone => Err(Close::Dropped("removed from the world")),
        }
    }

    /// Run one battle request or command against the world and answer a refusal to this client
    /// alone (a refusal is routine: a stale command, a debug request on a normal server).
    fn battle(&mut self, op: impl FnOnce(&mut World, Instant) -> BattleReply) -> Result<(), Close> {
        let reply = self.shared.with_world(|world| op(world, Instant::now()));
        match reply {
            BattleReply::Done => Ok(()),
            BattleReply::Refused { battle, reason } => {
                debug!(?battle, ?reason, "battle request refused");
                self.send(ServerMsg::BattleRejected { battle_id: battle, reason })
            }
            BattleReply::Gone => Err(Close::Dropped("removed from the world")),
        }
    }

    /// Read the next client message, honouring shutdown, the close flag, an optional timeout and
    /// the per-connection rate limit. The read is dropped mid-frame only when the connection is
    /// being torn down anyway, so losing partial data is harmless.
    async fn recv(&mut self, limit: Option<Duration>, on_timeout: OnTimeout) -> Result<ClientMsg, Close> {
        let rd = &mut self.rd;
        let shutdown = &mut self.shutdown;
        let close_rx = &mut self.close_rx;
        let read = async {
            let fut = read_frame(rd);
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
        let payload = match outcome {
            None => return Err(Close::reject(on_timeout.0, on_timeout.1)),
            Some(Ok(payload)) => payload,
            Some(Err(ProtoError::Closed | ProtoError::Io(_))) => return Err(Close::PeerGone),
            Some(Err(ProtoError::FrameTooLarge(n))) => {
                return Err(Close::reject(RejectCode::BadRequest, format!("frame too large ({n} bytes, maximum {MAX_FRAME})")));
            }
            // `read_frame` does not decode, so it cannot fail this way.
            Some(Err(ProtoError::Codec(_))) => return Err(Close::reject(RejectCode::BadRequest, "malformed message")),
        };
        let first = std::mem::replace(&mut self.first_message, false);
        let msg = match decode::<ClientMsg>(&payload) {
            Ok(msg) => msg,
            Err(_) => {
                // A Hello of another protocol version has another shape: say so, instead of
                // "malformed message" (the version is readable whatever else changed).
                if first {
                    if let Some(version) = probe_hello_version(&payload) {
                        if let Err(why) = check_version(version) {
                            return Err(Close::reject(RejectCode::ProtocolMismatch, why));
                        }
                    }
                }
                return Err(Close::reject(RejectCode::BadRequest, "malformed message"));
            }
        };
        if !self.limiter.allow(Instant::now()) {
            return Err(Close::reject(RejectCode::RateLimited, "rate limit exceeded"));
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
        let tick = self.shared.with_world(|world| world.current_tick());
        self.send(ServerMsg::Pong { nonce, tick })
    }

    /// The player pressed OK (`ClientMsg::Interact`). What it does is the world's (`World::press`); when a ladder moved
    /// the player the client is told where it stands now, with the message that already does that.
    fn on_interact(&mut self, entity: EntityId, npc: Option<u32>) -> Result<(), Close> {
        match self.shared.with_world(|world| world.press(entity, npc, Instant::now())) {
            PressOutcome::Nothing => Ok(()),
            PressOutcome::Relocated { corrected } => self.send(ServerMsg::MoveRejected { seq: self.last_move_seq, corrected }),
            PressOutcome::Gone => Err(Close::Dropped("removed from the world")),
        }
    }

    /// A scene message (`SceneStep`, `SceneEnd`, `SceneLeave`, `SceneDebugPreview`): the world decides what it does
    /// (`World::scene_message`); a refusal is told to the sender alone.
    fn on_scene(&mut self, entity: EntityId, msg: ClientMsg) -> Result<(), Close> {
        match self.shared.with_world(|world| world.scene_message(entity, msg, Instant::now())) {
            SceneReply::Done => Ok(()),
            SceneReply::Refused(reason) => self.send(ServerMsg::SceneRefused { reason }),
            SceneReply::Gone => Err(Close::Dropped("removed from the world")),
        }
    }

    fn on_move(&mut self, entity: EntityId, seq: u32, pos: Pos, facing: f32, anim: AnimState) -> Result<(), Close> {
        self.last_move_seq = seq;
        let outcome = self.shared.with_world(|world| world.apply_move(entity, pos, facing, anim, Instant::now()));
        match outcome {
            MoveOutcome::Applied => Ok(()),
            // Moved by a link (or its client has not taken that yet): the position the client is to take, which is
            // no refusal and is not counted or logged as one.
            MoveOutcome::Relocated { corrected } => self.send(ServerMsg::MoveRejected { seq, corrected }),
            MoveOutcome::Rejected { corrected, reason } => {
                self.rejected_moves += 1;
                if self.rejected_moves == 1 {
                    self.budgeted(
                        |_| info!(entity = entity.0, ?reason, "move rejected (further rejections on this connection are logged at debug)"),
                        || debug!(entity = entity.0, ?reason, "move rejected"),
                    );
                } else {
                    debug!(entity = entity.0, ?reason, "move rejected");
                }
                self.send(ServerMsg::MoveRejected { seq, corrected })
            }
            // In a battle the player is frozen: its intents are ignored, not answered.
            MoveOutcome::Frozen => Ok(()),
            MoveOutcome::Gone => Err(Close::Dropped("removed from the world")),
        }
    }

    /// Log why the connection ended. Reasons a stranger can cause at will (junk, refusals, a hang-up
    /// mid-handshake) go through the log throttle: a budget of INFO/WARN lines per address and in
    /// total, then `debug`, so a flood of junk connections cannot fill the disk.
    fn log_close(&self, reason: &Close) {
        match reason {
            Close::PeerGone => debug!("peer disconnected"),
            Close::Left => debug!("client left"),
            Close::Answered => debug!("answered a request that is not a session; closing connection"),
            Close::Shutdown => debug!("closing connection for shutdown"),
            Close::Reject(code, why) => self.budgeted(
                |suppressed| match suppressed {
                    0 => info!(?code, reason = %why, "closing connection"),
                    n => info!(?code, reason = %why, suppressed = n, "closing connection (earlier lines like this were throttled; see debug)"),
                },
                || debug!(?code, reason = %why, "closing connection (log throttled)"),
            ),
            Close::Dropped(why) => self.budgeted(
                |suppressed| match suppressed {
                    0 => warn!(why, "dropping connection"),
                    n => warn!(why, suppressed = n, "dropping connection (earlier lines like this were throttled; see debug)"),
                },
                || debug!(why, "dropping connection (log throttled)"),
            ),
        }
    }

    /// Write a line a stranger can cause at will within this peer's log budget: `loud(held_back)` at
    /// its normal level while the budget lasts (`held_back` lines of this kind went to `debug`
    /// since the last one), `quiet()` (the same line at `debug`) after, and one notice when the
    /// budget runs out.
    fn budgeted(&self, loud: impl FnOnce(u64), quiet: impl FnOnce()) {
        match self.shared.limits.logs.allow(Some(self.peer), Instant::now()) {
            LogVerdict::Emit { suppressed } => loud(suppressed),
            LogVerdict::FirstSuppressed => {
                throttle_notice();
                quiet();
            }
            LogVerdict::Suppress => quiet(),
        }
    }

    /// Tell the client why (if applicable), leave the world, flush and close. A registration whose
    /// `Welcome` never reached the socket is released here: nobody holds its token.
    async fn finish(mut self, reason: Close, mut writer: JoinHandle<()>) {
        self.log_close(&reason);
        let farewell = reason.farewell();
        if let Some((code, text)) = &farewell {
            // Best effort: if the queue is full this client is not going to read it anyway.
            let _ = self.tx.try_send(Arc::new(ServerMsg::reject(*code, text.clone())));
        }
        // Leaving the world drops the world's queue handle; dropping ours then closes the queue,
        // which lets the writer drain what is left and exit.
        drop(self.guard.take());
        drop(self.tx);
        let registered = self.registered.take();
        // A dropped connection's writer is not worth waiting for, unless a registration's
        // `Welcome` is in doubt: then the writer's count is the answer.
        let must_know = registered.is_some();
        if (matches!(reason, Close::Dropped(_)) && !must_know) || tokio::time::timeout(FLUSH_GRACE, &mut writer).await.is_err() {
            writer.abort();
        }
        if let Some(account) = undelivered(registered, self.written.load(Ordering::Acquire)) {
            release_undelivered(&self.shared, self.peer, account).await;
        }
        if reason.lingers() {
            linger(&mut self.rd).await;
        }
    }
}

/// Give back the account of a registration whose `Welcome` was never written, and say so (within
/// `peer`'s log budget).
async fn release_undelivered(shared: &Arc<Shared>, peer: PeerKey, account: AccountId) {
    let store = Arc::clone(&shared.store);
    let outcome = tokio::task::spawn_blocking(move || accounts::release_unclaimed(&store, account)).await;
    let verdict = shared.limits.logs.allow(Some(peer), Instant::now());
    match (outcome, verdict) {
        (Ok(Release::Released), LogVerdict::Emit { .. }) => {
            info!(account = account.0, "registration was never delivered: released, its display name is free again")
        }
        (Ok(Release::Released), LogVerdict::FirstSuppressed) => {
            throttle_notice();
            debug!(account = account.0, "undelivered registration released");
        }
        (Ok(Release::Released), LogVerdict::Suppress) => debug!(account = account.0, "undelivered registration released"),
        (Ok(Release::NotReleasable), _) => debug!(account = account.0, "undelivered registration was not releasable (already in use)"),
        (Ok(Release::Failed(cause)), _) => {
            warn!(account = account.0, %cause, "could not release a registration that was never delivered; its display name stays reserved")
        }
        (Err(e), _) => warn!(account = account.0, error = %e, "release of an undelivered registration failed"),
    }
}

/// Used by the accept loop when a connection cap is reached.
pub(crate) async fn reject_and_close(mut stream: TcpStream, code: RejectCode, reason: &str, write_timeout: Duration) {
    let msg = ServerMsg::reject(code, reason);
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
    fn only_the_current_protocol_version_is_accepted() {
        assert!(check_version(PROTOCOL_VERSION).is_ok());
        // The version before this one is refused too (v9 for v10: the target of a spell, an item or a limit changed shape).
        for other in [0, 1, PROTOCOL_VERSION - 1, PROTOCOL_VERSION + 1, u16::MAX] {
            let err = check_version(other).unwrap_err();
            assert!(err.contains("protocol version mismatch"), "{err}");
            assert!(err.contains(&format!("v{PROTOCOL_VERSION}")) && err.contains(&format!("v{other}")), "{err}");
        }
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

    /// Run `writer_task` over `wr` with one queued message and report what the default log showed,
    /// whether the close flag was raised and how many frames were completely written.
    /// (Current-thread runtime: the thread-local subscriber sees every event.)
    async fn writer_run<W: AsyncWrite + Unpin>(wr: W, msg: ServerMsg, write_timeout: Duration) -> (String, bool, u64) {
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
        let written = Arc::new(AtomicU64::new(0));
        writer_task(wr, rx, write_timeout, close, Arc::clone(&written)).await;
        let raised = *close_rx.borrow();
        let text = String::from_utf8_lossy(&logs.0.lock().unwrap()).into_owned();
        (text, raised, written.load(Ordering::Acquire))
    }

    #[tokio::test]
    async fn write_side_failures_are_logged_with_their_cause_at_the_default_level() {
        let hello = || ServerMsg::reject(RejectCode::Unspecified, "a message longer than the 16 byte pipe");

        // A message the protocol cannot carry (for example a full server's snapshot outgrowing
        // MAX_FRAME after a protocol change) must say so, not just "dropping connection".
        let too_big = ServerMsg::reject(RejectCode::Unspecified, "x".repeat(MAX_FRAME as usize));
        let (logs, closed, written) = writer_run(tokio::io::sink(), too_big, Duration::from_secs(5)).await;
        assert!(closed);
        assert_eq!(written, 0, "a frame that was not written is not counted");
        assert!(logs.contains("WARN") && logs.contains("frame too large"), "{logs}");

        // A client that stopped reading: its pipe fills up and the write times out.
        let (_reader_kept_open, pipe) = tokio::io::duplex(16);
        let (logs, closed, written) = writer_run(pipe, hello(), Duration::from_millis(50)).await;
        assert!(closed);
        assert_eq!(written, 0);
        assert!(logs.contains("WARN") && logs.contains("not reading"), "{logs}");

        // A peer that is simply gone is routine and stays out of the default log...
        let (reader, pipe) = tokio::io::duplex(16);
        drop(reader);
        let (logs, closed, written) = writer_run(pipe, hello(), Duration::from_secs(5)).await;
        assert!(closed);
        assert_eq!(written, 0, "nothing reached a peer that is gone");
        assert!(logs.is_empty(), "{logs}");

        // ...and so does an ordinary close after everything was delivered, which is counted.
        let (logs, closed, written) = writer_run(tokio::io::sink(), ServerMsg::Pong { nonce: 1, tick: 2 }, Duration::from_secs(5)).await;
        assert!(closed);
        assert_eq!(written, 1);
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
        let (code, text) = Close::Shutdown.farewell().expect("a stopping server says so");
        assert_eq!((code, text.as_str()), (RejectCode::ShuttingDown, "server shutting down"));
        let (code, text) = Close::reject(RejectCode::NameTaken, "x").farewell().expect("a refusal is told");
        assert_eq!((code, text.as_str()), (RejectCode::NameTaken, "x"));
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

    // ---- the registration and its token ------------------------------------------------------------

    /// A connected pair of real sockets: (client, the server's accepted end).
    async fn socket_pair() -> (TcpStream, TcpStream) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (client, accepted) = tokio::join!(TcpStream::connect(addr), listener.accept());
        (client.unwrap(), accepted.unwrap().0)
    }

    #[tokio::test]
    async fn a_peer_that_hung_up_or_reset_is_not_given_an_account() {
        // Silence and data are alive.
        let (mut client, server) = socket_pair().await;
        assert!(peer_still_connected(&server), "nothing sent yet is the normal case");
        tokio::io::AsyncWriteExt::write_all(&mut client, b"hello").await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(peer_still_connected(&server), "pipelined data is alive");
        // The look does not consume it.
        let mut rd = server;
        let mut got = [0u8; 5];
        tokio::io::AsyncReadExt::read_exact(&mut rd, &mut got).await.unwrap();
        assert_eq!(&got, b"hello");

        // An orderly close.
        let (client, server) = socket_pair().await;
        drop(client);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!peer_still_connected(&server), "a closed peer would never read a token");

        // A reset right after the peer's last message: what the audit's ghost client did after its
        // Hello. The server still reads what was sent (data survives the reset) and only then finds
        // that nobody is there to be answered.
        let (mut client, mut server) = socket_pair().await;
        tokio::io::AsyncWriteExt::write_all(&mut client, b"hello").await.unwrap();
        socket2::SockRef::from(&client).set_linger(Some(Duration::ZERO)).unwrap();
        drop(client);
        tokio::time::sleep(Duration::from_millis(50)).await;
        let mut got = [0u8; 5];
        tokio::io::AsyncReadExt::read_exact(&mut server, &mut got).await.unwrap();
        assert_eq!(&got, b"hello", "the message sent before the reset is still delivered");
        assert!(!peer_still_connected(&server), "a reset peer would never read a token");
    }

    #[test]
    fn only_a_registration_whose_welcome_was_not_written_is_released() {
        let account = Some(AccountId(7));
        assert_eq!(undelivered(account, 0), account, "not even the Welcome got out");
        assert_eq!(undelivered(account, 1), None, "the Welcome was written: the token is with its owner");
        assert_eq!(undelivered(account, 5), None);
        assert_eq!(undelivered(None, 0), None, "a token login has nothing to release");
    }

    // ---- idle sessions --------------------------------------------------------------------------

    #[test]
    fn a_session_is_cut_by_whichever_idle_clock_runs_out_first() {
        let s = Duration::from_secs;
        // Only silence is watched: the usual per-message window.
        assert_eq!(next_wait(Some(s(30)), None, s(500)), Wait::Read(Some(s(30)), SILENCE_TIMEOUT));
        // Nothing is watched.
        assert_eq!(next_wait(None, None, s(500)), Wait::Read(None, SILENCE_TIMEOUT));
        // Both: while the inactivity deadline is far away, silence is what can end the read.
        assert_eq!(next_wait(Some(s(30)), Some(s(1800)), s(10)), Wait::Read(Some(s(30)), SILENCE_TIMEOUT));
        // Close to it, the read ends at the inactivity deadline, with its own farewell.
        match next_wait(Some(s(30)), Some(s(1800)), s(1790)) {
            Wait::Read(Some(limit), (code, text)) => {
                assert_eq!(limit, s(10));
                assert_eq!(code, RejectCode::IdleTimeout);
                assert!(text.contains("inactivity"), "{text}");
            }
            other => panic!("{other:?}"),
        }
        // Silence checked by inactivity alone (no silence limit): still bounded.
        assert!(matches!(next_wait(None, Some(s(60)), s(1)), Wait::Read(Some(limit), _) if limit == s(59)));
        // Out of time: no read at all.
        assert_eq!(next_wait(Some(s(30)), Some(s(1800)), s(1800)), Wait::Inactive);
        assert_eq!(next_wait(Some(s(30)), Some(s(1800)), s(9999)), Wait::Inactive);
        // The farewell says what happened and what to do, in minutes for a long limit.
        match inactivity_farewell(s(1800)) {
            Close::Reject(code, text) => {
                assert_eq!(code, RejectCode::IdleTimeout);
                assert!(text.contains("30 minute(s)") && text.contains("pings do not count") && text.contains("log in again"), "{text}");
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(inactivity_farewell(s(5)), Close::Reject(_, text) if text.contains("5 second(s)")));
    }
}
