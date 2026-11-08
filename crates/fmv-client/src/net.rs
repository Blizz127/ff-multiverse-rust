//! Networking: a tokio runtime on a background thread, talking to the game loop over channels.
//!
//! * [`handshake`] is generic over the reader/writer so it can be tested with in-memory pipes.
//!   It tolerates `Welcome` / `AreaManifest` / `AreaSnapshot` arriving in any order, treats
//!   `Reject` as fatal at every stage and bounds everything by one overall deadline.
//! * A `Reject` becomes [`NetError::Rejected`], which keeps the server's [`RejectCode`]: what to
//!   tell the player and what to do about a refusal is decided from that code, never from the
//!   server's sentence (the sentence is shown, cleaned, but is free to change).
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
//! * Accounts: `Hello` carries an [`Auth`] (a saved token, or a display name to register). A
//!   registration is answered by a `Welcome` that carries the new account's token exactly once;
//!   the handshake hands it to the [`TokenSink`] the instant it arrives, before anything else can
//!   fail, because the registered name is unusable without it. The token is a [`Secret`]: it is
//!   never logged, and appears in text only in [`NetError::TokenNotSaved`], the one case where
//!   showing it is the user's only way to keep the account.

use crate::battle::BattleCmd;
use crate::cli::exit;
use crate::state::Intent;
use fmv_adapter_api::{AreaRuntime, GameAdapter};
use fmv_core::{AccountId, AreaId, EntityId, EntitySnapshot, Fingerprint, GameId, ProvenanceSummary, SessionId};
use fmv_protocol::{read_msg, write_msg, Auth, ClientMsg, InviteId, ProtoError, RejectCode, Secret, ServerMsg, PROTOCOL_VERSION};
use std::fmt;
use std::sync::Arc;
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

/// Where a freshly issued account token is stored. Called by the handshake the instant a
/// `Welcome` with `issued_token: Some(..)` arrives (see the module docs); an `Err` aborts the
/// handshake with [`NetError::TokenNotSaved`].
///
/// A small cloneable wrapper so [`NetConfig`] stays `Clone + Debug`: the closure is shared, and
/// its `Debug` shows nothing (it may capture paths, never the token).
#[derive(Clone)]
pub struct TokenSink(Arc<SaveToken>);

/// What a [`TokenSink`] runs.
type SaveToken = dyn Fn(&Secret) -> Result<(), String> + Send + Sync;

impl TokenSink {
    /// A sink that runs `save`. Its error text is shown to the user, so it must not contain the
    /// token (the handshake adds the token to the message itself, in that one case).
    pub fn new(save: impl Fn(&Secret) -> Result<(), String> + Send + Sync + 'static) -> Self {
        Self(Arc::new(save))
    }

    /// Store `token` (the handshake calls this; tests of the sinks do too).
    pub(crate) fn save(&self, token: &Secret) -> Result<(), String> {
        (self.0)(token)
    }
}

impl fmt::Debug for TokenSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TokenSink(..)")
    }
}

#[derive(Debug, Clone)]
pub struct NetConfig {
    pub server: String,
    /// How to authenticate: the saved token, or the display name to register.
    pub auth: Auth,
    /// The look requested for a NEW avatar (only used when `auth` is a registration; a login
    /// takes the stored profile).
    pub tint: [u8; 3],
    pub appearance_ref: Option<String>,
    pub client_build: String,
    pub timing: SessionTiming,
    /// Stores the token a registration issues.
    pub token_sink: TokenSink,
}

#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("cannot connect to {addr}: {source} (is the server running?)")]
    Connect { addr: String, source: std::io::Error },
    #[error("connecting to {0} timed out after {1} s")]
    ConnectTimeout(String, u64),
    /// The server refused the connection. `code` is what a program decides on (the hint for the
    /// player, whether a saved token is to blame); `reason` is the server's own sentence, already
    /// cleaned, which is only ever shown and never matched against. A server that sends no code
    /// (or one this build does not know) reads as [`RejectCode::Unspecified`].
    #[error("server rejected the connection: {reason}")]
    Rejected { code: RejectCode, reason: String },
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
    /// A registration succeeded, but the token it issued could not be stored. The token IS part
    /// of the text, on purpose and only here: it is shown once and the account's name is taken,
    /// so printing it is the user's only chance to keep the account. (`Debug` still redacts it.)
    #[error("the account was created on the server, but its token could not be saved ({reason}); save this token by hand, it is shown only once: {}", .token.expose())]
    TokenNotSaved { reason: String, token: Secret },
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
    /// The authenticated account (party events name accounts, so this is how the session
    /// recognises itself in them).
    pub account: AccountId,
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

/// A party request the player (or the headless bot) makes. Each one is sent as the matching
/// `ClientMsg`; the server decides and answers with `PartyEvent` / `PartyRejected`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartyCommand {
    /// Invite the player with this entity (into your party, or to form one).
    Invite(EntityId),
    /// Accept an invite addressed to you.
    Accept(InviteId),
    /// Decline an invite addressed to you. (No key or bot flag sends it yet; the protocol has it.)
    #[allow(dead_code)]
    Decline(InviteId),
    /// Leave your party.
    Leave,
    /// Leader only: remove a member. (No key or bot flag sends it yet; the protocol has it.)
    #[allow(dead_code)]
    Kick(EntityId),
    /// Leader only: hand leadership to a member. (No key or bot flag sends it yet.)
    #[allow(dead_code)]
    Promote(EntityId),
    /// Ask for the full party state (answered by `PartyState`).
    Sync,
}

impl PartyCommand {
    /// The message that carries this command.
    pub fn to_msg(self) -> ClientMsg {
        match self {
            PartyCommand::Invite(target) => ClientMsg::PartyInvite { target },
            PartyCommand::Accept(invite) => ClientMsg::PartyAccept { invite },
            PartyCommand::Decline(invite) => ClientMsg::PartyDecline { invite },
            PartyCommand::Leave => ClientMsg::PartyLeave,
            PartyCommand::Kick(target) => ClientMsg::PartyKick { target },
            PartyCommand::Promote(target) => ClientMsg::PartyPromote { target },
            PartyCommand::Sync => ClientMsg::PartySync,
        }
    }
}

/// The client's answer to an `AreaChange` (the server moves the player to another area): it holds
/// that area locally (`JoinArea`, with its OWN path-free provenance) or does not (`CannotJoin`, and
/// the server sends the player back).
#[derive(Debug, Clone, PartialEq)]
pub enum AreaAnswer {
    Join { area: AreaId, fingerprint: Fingerprint, provenance: ProvenanceSummary },
    /// `reason` is path-free text (see [`ContentMismatch::wire`]).
    Cannot { area: AreaId, reason: String },
}

impl AreaAnswer {
    /// The message that carries this answer.
    pub fn into_msg(self) -> ClientMsg {
        match self {
            AreaAnswer::Join { area, fingerprint, provenance } => ClientMsg::JoinArea { area, fingerprint, provenance },
            AreaAnswer::Cannot { area, reason } => ClientMsg::CannotJoin { area, reason },
        }
    }
}

/// Messages from the game loop to the network thread.
#[derive(Debug, Clone)]
pub enum NetCommand {
    Intent(Intent),
    Party(PartyCommand),
    Battle(BattleCmd),
    /// The answer to an `AreaChange`.
    Area(AreaAnswer),
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

/// What a successful local content check yields.
pub struct LocalContent {
    /// The area decoded from this machine's own content.
    pub runtime: Box<dyn AreaRuntime>,
    /// This machine's OWN path-free provenance of that area (what `JoinArea` sends; the server
    /// requires it to be `compatible_with` the area's, in addition to the fingerprint matching).
    pub provenance: ProvenanceSummary,
}

/// Compare what the server offers with what this client holds locally. On success returns the
/// area decoded from local content together with the local provenance; on failure a concrete
/// reason for `CannotJoin` (and the fuller one for the user).
pub fn check_local_content(
    adapter: &dyn GameAdapter,
    area: &AreaId,
    fingerprint: &Fingerprint,
    provenance: &ProvenanceSummary,
) -> Result<LocalContent, ContentMismatch> {
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
    Ok(LocalContent { runtime, provenance: local })
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
    account: AccountId,
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
///
/// A `Welcome` that carries a newly issued token is handed to `cfg.token_sink` first of all: before
/// the version check, the local content check and everything else that can fail.
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
        auth: cfg.auth.clone(),
        tint: cfg.tint,
        appearance_ref: cfg.appearance_ref.clone(),
    };
    send(wr, &hello, "sending Hello", deadline).await?;

    let mut welcome: Option<Welcome> = None;
    let mut offer: Option<Offer> = None;
    let mut snapshot: Option<Vec<EntitySnapshot>> = None;
    let mut backlog: Vec<ServerMsg> = Vec::new();
    // The sink is called once per handshake, however many Welcomes a broken server sends.
    let mut token_handled = false;

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
            ServerMsg::Reject { reason, code } => return Err(NetError::Rejected { code, reason: clean(&reason) }),
            ServerMsg::Welcome {
                protocol_version,
                server_build,
                session,
                entity,
                account,
                issued_token,
            } => {
                // FIRST, before any check that could fail: a registered name is unique and cannot
                // be recovered without this token, which the server shows exactly once.
                if let (Some(token), false) = (&issued_token, token_handled) {
                    token_handled = true;
                    if let Err(reason) = cfg.token_sink.save(token) {
                        return Err(NetError::TokenNotSaved {
                            reason: clean_to(&reason, MAX_LOCAL_DETAIL),
                            token: token.clone(),
                        });
                    }
                }
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
                    account,
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
                    Ok(LocalContent { runtime, provenance: local_provenance }) => {
                        // Our OWN path-free provenance (not an echo of the server's): the server
                        // requires it to be compatible with the area's.
                        let join = ClientMsg::JoinArea {
                            area: area.clone(),
                            fingerprint,
                            provenance: local_provenance,
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
            ServerMsg::AreaSnapshot { area, tick, entities, npcs } => {
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
                push_backlog(&mut backlog, ServerMsg::AreaSnapshot { area, tick, entities, npcs });
            }
            // A transfer between areas is something that happens in the game, never in the
            // handshake: honouring one here would skip the checks of the first area.
            ServerMsg::AreaChange { .. } => {
                return Err(NetError::Protocol("AreaChange during the handshake".into()));
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
            // Party traffic does not depend on the area: it is kept whenever it arrives, so the
            // party mirror never misses an event (a gap in `seq` would only cost a resync). The
            // player's own numbers (`CharacterProgress`, and the `Inventory` that follows it on
            // joining, protocol v6) are kept for the same reason: they are said once, not repeated.
            other @ (ServerMsg::InstancePlacement { .. }
            | ServerMsg::PartyEvent { .. }
            | ServerMsg::PartyState { .. }
            | ServerMsg::PartyRejected { .. }
            | ServerMsg::EncounterStart { .. }
            | ServerMsg::BattleEvent { .. }
            | ServerMsg::BattleEnd { .. }
            | ServerMsg::BattleRejected { .. }
            | ServerMsg::CharacterProgress { .. }
            | ServerMsg::Inventory { .. }) => push_backlog(&mut backlog, other),
        }
        if let (Some(_), Some(_), Some(_)) = (&welcome, &offer, &snapshot) {
            let (Some(w), Some(o), Some(entities)) = (welcome, offer, snapshot) else {
                return Err(NetError::Protocol("handshake state lost".into()));
            };
            return Ok(Joined {
                session: w.session,
                entity: w.entity,
                account: w.account,
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
                Some(NetCommand::Party(p)) => p.to_msg(),
                Some(NetCommand::Battle(b)) => b.to_msg(),
                Some(NetCommand::Area(a)) => a.into_msg(),
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
            auth: Auth::Register { display_name: "tester".into() },
            tint: [1, 2, 3],
            appearance_ref: None,
            client_build: "test".into(),
            timing: SessionTiming::DEFAULT,
            token_sink: TokenSink::new(|_| Ok(())),
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
        welcome_with(None)
    }

    fn welcome_with(issued_token: Option<&str>) -> ServerMsg {
        ServerMsg::Welcome {
            protocol_version: PROTOCOL_VERSION,
            server_build: "srv".into(),
            session: SessionId(5),
            entity: EntityId(7),
            account: AccountId(42),
            issued_token: issued_token.map(Secret::new),
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
            npcs: Vec::new(),
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
                auth,
                tint,
                ..
            } = hello
            else {
                panic!("expected Hello")
            };
            assert_eq!(
                (protocol_version, auth, tint),
                (PROTOCOL_VERSION, Auth::Register { display_name: "tester".into() }, [1, 2, 3])
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
        assert_eq!(joined.account, AccountId(42), "the Welcome's account is returned");
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
                write_msg(&mut swr, &ServerMsg::reject(RejectCode::ServerFull, "server full")).await.unwrap();
            });
            let err = handshake(&mut crd, &mut cwr, &cfg(), &adapter, Duration::from_secs(5))
                .await
                .err()
                .expect("must fail");
            server_task.await.unwrap();
            assert!(
                matches!(&err, NetError::Rejected { code: RejectCode::ServerFull, reason } if reason == "server full"),
                "stage {stage}: {err}"
            );
            assert_eq!(err.exit_code(), exit::FAILURE);
        }
    }

    /// Every code survives the trip from the wire to the error unchanged (callers decide on it),
    /// and the sentence that travels with it is cleaned like all server text.
    #[tokio::test]
    async fn a_reject_keeps_its_code_and_a_cleaned_reason() {
        for code in RejectCode::ALL {
            let adapter = FixtureAdapter::new();
            let (client, server) = duplex(4096);
            let (mut crd, mut cwr) = split(client);
            let (mut srd, mut swr) = split(server);
            let server_task = tokio::spawn(async move {
                let _hello: ClientMsg = read_msg(&mut srd).await.unwrap();
                write_msg(&mut swr, &ServerMsg::reject(code, "no\u{1b}[31m way\n")).await.unwrap();
            });
            let err = handshake(&mut crd, &mut cwr, &cfg(), &adapter, Duration::from_secs(5)).await.err().expect("must fail");
            server_task.await.unwrap();
            assert!(
                matches!(&err, NetError::Rejected { code: got, reason } if *got == code && reason == "no[31m way"),
                "{code:?}: {err}"
            );
            assert_eq!(err.to_string(), "server rejected the connection: no[31m way", "{code:?}");
        }
    }

    /// A server without codes (an older build) or with a code this build does not know still gets
    /// its refusal through, as `Unspecified` with its words intact. The frames are written by hand
    /// so they are exactly what such a server sends, whatever `ServerMsg` serializes today.
    #[tokio::test]
    async fn a_reject_without_a_known_code_reads_as_unspecified() {
        for payload in [
            r#"{"Reject":{"reason":"server full"}}"#,
            r#"{"Reject":{"reason":"server full","code":"SomethingNew"}}"#,
        ] {
            let adapter = FixtureAdapter::new();
            let (client, server) = duplex(4096);
            let (mut crd, mut cwr) = split(client);
            let (mut srd, mut swr) = split(server);
            let server_task = tokio::spawn(async move {
                let _hello: ClientMsg = read_msg(&mut srd).await.unwrap();
                // A frame is a little-endian u32 length and the JSON payload.
                swr.write_all(&u32::try_from(payload.len()).unwrap().to_le_bytes()).await.unwrap();
                swr.write_all(payload.as_bytes()).await.unwrap();
            });
            let err = handshake(&mut crd, &mut cwr, &cfg(), &adapter, Duration::from_secs(5)).await.err().expect("must fail");
            server_task.await.unwrap();
            assert!(
                matches!(&err, NetError::Rejected { code: RejectCode::Unspecified, reason } if reason == "server full"),
                "{payload}: {err}"
            );
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
                account: AccountId(1),
                issued_token: None,
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

    // ------------------------------------------------------------------------------------
    // Accounts: the token, the authentication, the provenance
    // ------------------------------------------------------------------------------------

    /// A sink that records every token it is given (and what `result` says), plus a `Notify` that
    /// fires on every call.
    fn recording_sink(result: Result<(), String>) -> (TokenSink, Arc<std::sync::Mutex<Vec<String>>>, Arc<tokio::sync::Notify>) {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let notify = Arc::new(tokio::sync::Notify::new());
        let sink = {
            let (seen, notify) = (Arc::clone(&seen), Arc::clone(&notify));
            TokenSink::new(move |t: &Secret| {
                seen.lock().expect("lock").push(t.expose().to_string());
                notify.notify_one();
                result.clone()
            })
        };
        (sink, seen, notify)
    }

    fn cfg_with(sink: TokenSink) -> NetConfig {
        NetConfig { token_sink: sink, ..cfg() }
    }

    /// The token reaches the sink when its `Welcome` arrives, not later: the server holds back
    /// everything else until the sink has been called.
    #[tokio::test]
    async fn the_token_is_saved_the_instant_the_welcome_arrives() {
        let adapter = FixtureAdapter::new();
        let (client, server) = duplex(64 * 1024);
        let (mut crd, mut cwr) = split(client);
        let (mut srd, mut swr) = split(server);
        let (sink, seen, notify) = recording_sink(Ok(()));
        let (srv_adapter, srv_notify) = (FixtureAdapter::new(), Arc::clone(&notify));
        let server_task = tokio::spawn(async move {
            let _hello: ClientMsg = read_msg(&mut srd).await.unwrap();
            write_msg(&mut swr, &welcome_with(Some("fmvt_issued"))).await.unwrap();
            // Nothing else is sent until the sink was called: if the client waited for the
            // manifest (or anything else) before saving, this times out.
            timeout(Duration::from_secs(5), srv_notify.notified())
                .await
                .expect("the token must be saved as soon as the Welcome arrives");
            write_msg(&mut swr, &manifest(&srv_adapter, |_| {})).await.unwrap();
            let _join: ClientMsg = read_msg(&mut srd).await.unwrap();
            write_msg(&mut swr, &area_snapshot(&srv_adapter, vec![snap(7, 0.0)])).await.unwrap();
        });
        let joined = handshake(&mut crd, &mut cwr, &cfg_with(sink), &adapter, Duration::from_secs(10))
            .await
            .unwrap();
        server_task.await.unwrap();
        assert_eq!(joined.account, AccountId(42));
        assert_eq!(*seen.lock().unwrap(), vec!["fmvt_issued".to_string()], "saved once");
    }

    /// Prove "before the content check": the area offered cannot be joined (a mismatching
    /// fingerprint), the handshake fails with a content error, and the token was saved anyway.
    #[tokio::test]
    async fn the_token_is_saved_before_the_content_check_can_fail() {
        let adapter = FixtureAdapter::new();
        let (client, server) = duplex(64 * 1024);
        let (mut crd, mut cwr) = split(client);
        let (mut srd, mut swr) = split(server);
        let (sink, seen, _notify) = recording_sink(Ok(()));
        let srv_adapter = FixtureAdapter::new();
        let server_task = tokio::spawn(async move {
            let _hello: ClientMsg = read_msg(&mut srd).await.unwrap();
            write_msg(&mut swr, &welcome_with(Some("fmvt_before_content"))).await.unwrap();
            let tampered = manifest(&srv_adapter, |m| {
                if let ServerMsg::AreaManifest { fingerprint, .. } = m {
                    fingerprint.0[0] ^= 0xff;
                }
            });
            write_msg(&mut swr, &tampered).await.unwrap();
            read_msg::<_, ClientMsg>(&mut srd).await.unwrap()
        });
        let err = handshake(&mut crd, &mut cwr, &cfg_with(sink), &adapter, Duration::from_secs(5))
            .await
            .err()
            .expect("the content check fails");
        assert!(matches!(&err, NetError::Content(r) if r.contains("fingerprint mismatch")), "{err}");
        assert!(matches!(server_task.await.unwrap(), ClientMsg::CannotJoin { .. }));
        assert_eq!(*seen.lock().unwrap(), vec!["fmvt_before_content".to_string()], "the token was saved first");
        assert!(!err.to_string().contains("fmvt_before_content"), "an ordinary failure never shows the token: {err}");
    }

    /// The same for the other checks that can fail on a `Welcome`: a wrong protocol version and
    /// a duplicate (the sink is called once however many Welcomes arrive).
    #[tokio::test]
    async fn the_token_is_saved_before_the_version_check_and_only_once() {
        let adapter = FixtureAdapter::new();
        // Wrong protocol version, with a token.
        let (client, server) = duplex(4096);
        let (mut crd, mut cwr) = split(client);
        let (_srd, mut swr) = split(server);
        let (sink, seen, _notify) = recording_sink(Ok(()));
        let ServerMsg::Welcome { server_build, session, entity, account, issued_token, .. } = welcome_with(Some("fmvt_v99")) else {
            panic!("a Welcome")
        };
        let wrong = ServerMsg::Welcome { protocol_version: 99, server_build, session, entity, account, issued_token };
        write_msg(&mut swr, &wrong).await.unwrap();
        let err = handshake(&mut crd, &mut cwr, &cfg_with(sink), &adapter, Duration::from_secs(5)).await.err().unwrap();
        assert!(matches!(&err, NetError::Protocol(m) if m.contains("v99")), "{err}");
        assert_eq!(*seen.lock().unwrap(), vec!["fmvt_v99".to_string()]);

        // Two Welcomes with tokens: one call, then the duplicate is a protocol error.
        let (client, server) = duplex(4096);
        let (mut crd, mut cwr) = split(client);
        let (_srd, mut swr) = split(server);
        let (sink, seen, _notify) = recording_sink(Ok(()));
        write_msg(&mut swr, &welcome_with(Some("fmvt_first"))).await.unwrap();
        write_msg(&mut swr, &welcome_with(Some("fmvt_second"))).await.unwrap();
        let err = handshake(&mut crd, &mut cwr, &cfg_with(sink), &adapter, Duration::from_secs(5)).await.err().unwrap();
        assert!(matches!(&err, NetError::Protocol(m) if m.contains("duplicate Welcome")), "{err}");
        assert_eq!(*seen.lock().unwrap(), vec!["fmvt_first".to_string()], "the second token is not offered to the sink");
    }

    /// A login (no token issued) never calls the sink.
    #[tokio::test]
    async fn no_token_means_no_sink_call() {
        let adapter = FixtureAdapter::new();
        let (client, server) = duplex(64 * 1024);
        let (mut crd, mut cwr) = split(client);
        let (mut srd, mut swr) = split(server);
        let (sink, seen, _notify) = recording_sink(Ok(()));
        let srv_adapter = FixtureAdapter::new();
        let server_task = tokio::spawn(async move {
            let _hello: ClientMsg = read_msg(&mut srd).await.unwrap();
            write_msg(&mut swr, &welcome()).await.unwrap();
            write_msg(&mut swr, &manifest(&srv_adapter, |_| {})).await.unwrap();
            let _join: ClientMsg = read_msg(&mut srd).await.unwrap();
            write_msg(&mut swr, &area_snapshot(&srv_adapter, vec![snap(7, 0.0)])).await.unwrap();
        });
        let mut config = cfg_with(sink);
        config.auth = Auth::Token(Secret::new("fmvt_saved"));
        handshake(&mut crd, &mut cwr, &config, &adapter, Duration::from_secs(5)).await.unwrap();
        server_task.await.unwrap();
        assert!(seen.lock().unwrap().is_empty());
    }

    /// If the token cannot be saved the handshake stops there: the account exists on the server,
    /// so the user must be told, with the token, and nothing else is sent.
    #[tokio::test]
    async fn a_failing_sink_aborts_the_handshake_and_shows_the_token_once() {
        let adapter = FixtureAdapter::new();
        let (client, server) = duplex(64 * 1024);
        let (mut crd, mut cwr) = split(client);
        let (mut srd, mut swr) = split(server);
        let (sink, seen, _notify) = recording_sink(Err("no space left on device".into()));
        let srv_adapter = FixtureAdapter::new();
        let server_task = tokio::spawn(async move {
            let _hello: ClientMsg = read_msg(&mut srd).await.unwrap();
            write_msg(&mut swr, &welcome_with(Some("fmvt_unsaved_secret"))).await.unwrap();
            let _ = write_msg(&mut swr, &manifest(&srv_adapter, |_| {})).await;
            // The client must not answer the manifest: it gives up, and the connection closes.
            read_msg::<_, ClientMsg>(&mut srd).await
        });
        let err = handshake(&mut crd, &mut cwr, &cfg_with(sink), &adapter, Duration::from_secs(5))
            .await
            .err()
            .expect("an unsaved token aborts the handshake");
        drop((crd, cwr));
        assert!(matches!(server_task.await.unwrap(), Err(ProtoError::Closed)), "nothing was sent after the Welcome");
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert_eq!(err.exit_code(), exit::FAILURE);
        // The user is told what happened, why, and gets the token (the one place it is shown).
        let text = err.to_string();
        assert!(text.contains("created on the server") && text.contains("could not be saved"), "{text}");
        assert!(text.contains("no space left on device"), "{text}");
        assert!(text.contains("fmvt_unsaved_secret"), "the token is shown so it can be saved by hand: {text}");
        // ...but never through Debug (logs, assertion messages).
        assert!(!format!("{err:?}").contains("fmvt_unsaved_secret"), "{err:?}");
    }

    /// `Hello` carries the authentication exactly as configured, tint and appearance included.
    #[tokio::test]
    async fn hello_carries_the_configured_auth() {
        let adapter = FixtureAdapter::new();
        for auth in [Auth::Token(Secret::new("fmvt_saved_token")), Auth::Register { display_name: "Cloud".into() }] {
            let (client, server) = duplex(4096);
            let (mut crd, mut cwr) = split(client);
            let (mut srd, mut swr) = split(server);
            let server_task = tokio::spawn(async move {
                let hello: ClientMsg = read_msg(&mut srd).await.unwrap();
                write_msg(&mut swr, &ServerMsg::reject(RejectCode::ShuttingDown, "stop here")).await.unwrap();
                hello
            });
            let mut config = cfg();
            config.auth = auth.clone();
            config.appearance_ref = Some("cloud_model".into());
            config.client_build = "fmv-client/test".into();
            let err = handshake(&mut crd, &mut cwr, &config, &adapter, Duration::from_secs(5)).await.err().unwrap();
            assert!(matches!(err, NetError::Rejected { code: RejectCode::ShuttingDown, .. }));
            match server_task.await.unwrap() {
                ClientMsg::Hello { protocol_version, client_build, auth: sent, tint, appearance_ref } => {
                    assert_eq!(protocol_version, PROTOCOL_VERSION);
                    assert_eq!(client_build, "fmv-client/test");
                    assert_eq!(sent, auth);
                    assert_eq!((tint, appearance_ref.as_deref()), ([1, 2, 3], Some("cloud_model")));
                }
                other => panic!("expected Hello, got {other:?}"),
            }
        }
    }

    /// The fixture's catalog with another language and the `modded` flag: both are part of the
    /// provenance but not of `compatible_with`, so a server still accepts such a client.
    struct Relabeled(FixtureAdapter);

    impl GameAdapter for Relabeled {
        fn game(&self) -> GameId {
            self.0.game()
        }
        fn list_areas(&self) -> Vec<AreaInfo> {
            self.0
                .list_areas()
                .into_iter()
                .map(|mut a| {
                    a.provenance.language = "xx-local".into();
                    a.provenance.modded = true;
                    a
                })
                .collect()
        }
        fn load_area(&self, key: &str) -> Result<Box<dyn AreaRuntime>, AdapterError> {
            self.0.load_area(key)
        }
    }

    /// `JoinArea` carries THIS client's own path-free provenance, not an echo of the server's.
    #[tokio::test]
    async fn join_area_carries_the_local_provenance() {
        let local = Relabeled(FixtureAdapter::new());
        let (client, server) = duplex(64 * 1024);
        let (mut crd, mut cwr) = split(client);
        let (mut srd, mut swr) = split(server);
        let srv_adapter = FixtureAdapter::new();
        let server_task = tokio::spawn(async move {
            let _hello: ClientMsg = read_msg(&mut srd).await.unwrap();
            write_msg(&mut swr, &welcome()).await.unwrap();
            write_msg(&mut swr, &manifest(&srv_adapter, |_| {})).await.unwrap();
            let join: ClientMsg = read_msg(&mut srd).await.unwrap();
            write_msg(&mut swr, &area_snapshot(&srv_adapter, vec![snap(7, 0.0)])).await.unwrap();
            join
        });
        handshake(&mut crd, &mut cwr, &cfg(), &local, Duration::from_secs(5)).await.unwrap();
        let ClientMsg::JoinArea { area, fingerprint, provenance } = server_task.await.unwrap() else {
            panic!("expected JoinArea")
        };
        let info = local.list_areas().remove(0);
        assert_eq!((area, fingerprint), (info.id.clone(), info.fingerprint));
        assert_eq!(provenance, info.provenance.summary(), "the client's own provenance");
        assert_eq!((provenance.language.as_str(), provenance.modded), ("xx-local", true));
        let offered = FixtureAdapter::new().list_areas().remove(0).provenance.summary();
        assert_ne!(provenance, offered, "it is not a copy of what the server offered");
        assert!(provenance.compatible_with(&offered), "but it is compatible with it, which is what the server checks");
        // No local path can be in it: it is the path-free summary type.
        assert!(!format!("{provenance:?}").contains('/'));
    }

    /// The content check hands back the local provenance together with the runtime.
    #[test]
    fn the_content_check_returns_the_local_provenance() {
        let adapter = Relabeled(FixtureAdapter::new());
        let info = adapter.list_areas().remove(0);
        let offered = FixtureAdapter::new().list_areas().remove(0).provenance.summary();
        let ok = check_local_content(&adapter, &info.id, &info.fingerprint, &offered).expect("compatible");
        assert_eq!(ok.provenance, info.provenance.summary());
        assert_eq!(ok.runtime.info().fingerprint, info.fingerprint);
    }

    /// Party traffic that arrives while the handshake completes is kept, in order, wherever it
    /// falls (even before the manifest); `InstancePlacement` after the snapshot is the session's.
    #[tokio::test]
    async fn party_messages_during_the_handshake_are_queued_in_order() {
        use fmv_protocol::{PartyEvent, PartyFailure};
        let adapter = FixtureAdapter::new();
        let (client, server) = duplex(64 * 1024);
        let (mut crd, mut cwr) = split(client);
        let (mut srd, mut swr) = split(server);
        let srv_adapter = FixtureAdapter::new();
        let event = |seq: u64| ServerMsg::PartyEvent {
            seq,
            event: PartyEvent::InviteExpired { invite: fmv_protocol::InviteId(seq), from: AccountId(1), to: AccountId(2) },
            names: vec![],
        };
        let server_task = tokio::spawn(async move {
            let _hello: ClientMsg = read_msg(&mut srd).await.unwrap();
            write_msg(&mut swr, &welcome()).await.unwrap();
            write_msg(&mut swr, &event(1)).await.unwrap(); // before the manifest
            write_msg(&mut swr, &manifest(&srv_adapter, |_| {})).await.unwrap();
            write_msg(&mut swr, &ServerMsg::PartyRejected { error: PartyFailure::RateLimited }).await.unwrap();
            let _join: ClientMsg = read_msg(&mut srd).await.unwrap();
            write_msg(&mut swr, &event(2)).await.unwrap();
            write_msg(&mut swr, &area_snapshot(&srv_adapter, vec![snap(7, 0.0)])).await.unwrap();
        });
        let joined = handshake(&mut crd, &mut cwr, &cfg(), &adapter, Duration::from_secs(5)).await.unwrap();
        server_task.await.unwrap();
        let kinds: Vec<&str> = joined
            .backlog
            .iter()
            .map(|m| match m {
                ServerMsg::PartyEvent { seq: 1, .. } => "event 1",
                ServerMsg::PartyEvent { seq: 2, .. } => "event 2",
                ServerMsg::PartyRejected { .. } => "rejected",
                ServerMsg::AreaSnapshot { .. } => "snapshot",
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(kinds, ["event 1", "rejected", "event 2", "snapshot"]);
    }

    /// What the server says about our own character when we join (the progress of protocol v5, then the
    /// inventory of v6) may reach us before the snapshot completes the handshake. It is said once and
    /// never repeated, so it is kept, in order, for the game loop; dropping it would leave the player
    /// with a character at level 0 and an empty bag.
    #[tokio::test]
    async fn our_progress_and_inventory_during_the_handshake_are_kept_in_order() {
        use fmv_protocol::InventoryEntry;
        let adapter = FixtureAdapter::new();
        let (client, server) = duplex(64 * 1024);
        let (mut crd, mut cwr) = split(client);
        let (mut srd, mut swr) = split(server);
        let srv_adapter = FixtureAdapter::new();
        let progress = ServerMsg::CharacterProgress { character: 0, level: 6, exp: 610, exp_to_next: 6, gil: 0, ap: 0, leveled_up: false };
        let inventory = ServerMsg::Inventory { items: vec![InventoryEntry::new(0, 3), InventoryEntry::new(7, 1)] };
        let (sent_progress, sent_inventory) = (progress.clone(), inventory.clone());
        let server_task = tokio::spawn(async move {
            let _hello: ClientMsg = read_msg(&mut srd).await.unwrap();
            write_msg(&mut swr, &welcome()).await.unwrap();
            write_msg(&mut swr, &manifest(&srv_adapter, |_| {})).await.unwrap();
            let _join: ClientMsg = read_msg(&mut srd).await.unwrap();
            write_msg(&mut swr, &sent_progress).await.unwrap();
            write_msg(&mut swr, &sent_inventory).await.unwrap();
            write_msg(&mut swr, &area_snapshot(&srv_adapter, vec![snap(7, 0.0)])).await.unwrap();
        });
        let joined = handshake(&mut crd, &mut cwr, &cfg(), &adapter, Duration::from_secs(5)).await.unwrap();
        server_task.await.unwrap();
        assert_eq!(joined.backlog.len(), 3, "{:?}", joined.backlog);
        assert_eq!(joined.backlog[0], progress);
        assert_eq!(joined.backlog[1], inventory);
        assert!(matches!(joined.backlog[2], ServerMsg::AreaSnapshot { .. }));
    }

    /// A transfer between areas is something that happens in the game: one offered in the middle of
    /// the handshake would skip the checks of the first area, so it ends the handshake.
    #[tokio::test]
    async fn an_area_change_during_the_handshake_is_a_protocol_error() {
        let adapter = FixtureAdapter::new();
        let info = adapter.list_areas().remove(0);
        let (client, server) = duplex(64 * 1024);
        let (mut crd, mut cwr) = split(client);
        let (_srd, mut swr) = split(server);
        write_msg(&mut swr, &welcome()).await.unwrap();
        write_msg(&mut swr, &manifest(&adapter, |_| {})).await.unwrap();
        write_msg(
            &mut swr,
            &ServerMsg::AreaChange {
                area: info.id.clone(),
                display_name: info.display_name.clone(),
                fingerprint: info.fingerprint,
                provenance: info.provenance.summary(),
                fixture: true,
                arrival: Pos::default(),
                arrival_facing: 0.0,
            },
        )
        .await
        .unwrap();
        let err = handshake(&mut crd, &mut cwr, &cfg(), &adapter, Duration::from_secs(5)).await.err().unwrap();
        assert!(matches!(&err, NetError::Protocol(m) if m.contains("AreaChange during the handshake")), "{err}");
    }

    #[test]
    fn area_answers_become_the_matching_messages() {
        let info = FixtureAdapter::new().list_areas().remove(0);
        let join = AreaAnswer::Join { area: info.id.clone(), fingerprint: info.fingerprint, provenance: info.provenance.summary() };
        assert_eq!(
            join.into_msg(),
            ClientMsg::JoinArea { area: info.id.clone(), fingerprint: info.fingerprint, provenance: info.provenance.summary() }
        );
        let cannot = AreaAnswer::Cannot { area: info.id.clone(), reason: "no such area here".into() };
        assert_eq!(cannot.into_msg(), ClientMsg::CannotJoin { area: info.id, reason: "no such area here".into() });
    }

    /// The session loop sends the answer to an `AreaChange` like any other command: in order with
    /// the rest, before the `Leave` that follows it.
    #[tokio::test]
    async fn the_session_loop_sends_area_answers_in_order() {
        let info = FixtureAdapter::new().list_areas().remove(0);
        let (client, server) = duplex(64 * 1024);
        let (crd, cwr) = split(client);
        let (mut srd, _swr) = split(server);
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (evt_tx, _evt_rx) = mpsc::channel(64);
        let session = tokio::spawn(session_loop(crd, cwr, cmd_rx, evt_tx, Instant::now(), SessionTiming::DEFAULT));
        cmd_tx.send(NetCommand::Area(AreaAnswer::Cannot { area: info.id.clone(), reason: "no".into() })).unwrap();
        cmd_tx
            .send(NetCommand::Area(AreaAnswer::Join { area: info.id.clone(), fingerprint: info.fingerprint, provenance: info.provenance.summary() }))
            .unwrap();
        cmd_tx.send(NetCommand::Leave).unwrap();
        let mut got = Vec::new();
        loop {
            match timeout(Duration::from_secs(10), read_msg::<_, ClientMsg>(&mut srd)).await.expect("a message") {
                Ok(ClientMsg::Ping { .. }) => {}
                Ok(msg) => {
                    let done = msg == ClientMsg::Leave;
                    got.push(msg);
                    if done {
                        break;
                    }
                }
                Err(e) => panic!("{e}"),
            }
        }
        assert!(matches!(&got[..], [ClientMsg::CannotJoin { .. }, ClientMsg::JoinArea { .. }, ClientMsg::Leave]), "{got:?}");
        session.await.unwrap();
    }

    #[test]
    fn party_commands_become_the_matching_messages() {
        use fmv_protocol::InviteId;
        let cases = [
            (PartyCommand::Invite(EntityId(8)), ClientMsg::PartyInvite { target: EntityId(8) }),
            (PartyCommand::Accept(InviteId(3)), ClientMsg::PartyAccept { invite: InviteId(3) }),
            (PartyCommand::Decline(InviteId(4)), ClientMsg::PartyDecline { invite: InviteId(4) }),
            (PartyCommand::Leave, ClientMsg::PartyLeave),
            (PartyCommand::Kick(EntityId(9)), ClientMsg::PartyKick { target: EntityId(9) }),
            (PartyCommand::Promote(EntityId(10)), ClientMsg::PartyPromote { target: EntityId(10) }),
            (PartyCommand::Sync, ClientMsg::PartySync),
        ];
        for (command, msg) in cases {
            assert_eq!(command.to_msg(), msg, "{command:?}");
        }
    }

    /// The session loop sends party commands as their messages, in order with everything else.
    #[tokio::test]
    async fn the_session_loop_sends_party_commands() {
        use fmv_protocol::InviteId;
        let (client, server) = duplex(64 * 1024);
        let (crd, cwr) = split(client);
        let (mut srd, _swr) = split(server);
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (evt_tx, _evt_rx) = mpsc::channel(64);
        let session = tokio::spawn(session_loop(crd, cwr, cmd_rx, evt_tx, Instant::now(), SessionTiming::DEFAULT));
        for command in [
            PartyCommand::Invite(EntityId(8)),
            PartyCommand::Accept(InviteId(3)),
            PartyCommand::Leave,
            PartyCommand::Sync,
        ] {
            cmd_tx.send(NetCommand::Party(command)).unwrap();
        }
        cmd_tx.send(NetCommand::Leave).unwrap();
        let mut got = Vec::new();
        loop {
            match timeout(Duration::from_secs(10), read_msg::<_, ClientMsg>(&mut srd)).await.expect("a message") {
                Ok(ClientMsg::Ping { .. }) => {}
                Ok(msg) => {
                    let done = msg == ClientMsg::Leave;
                    got.push(msg);
                    if done {
                        break;
                    }
                }
                Err(e) => panic!("{e}"),
            }
        }
        assert_eq!(
            got,
            vec![
                ClientMsg::PartyInvite { target: EntityId(8) },
                ClientMsg::PartyAccept { invite: InviteId(3) },
                ClientMsg::PartyLeave,
                ClientMsg::PartySync,
                ClientMsg::Leave,
            ]
        );
        timeout(Duration::from_secs(10), session).await.expect("the loop ends").unwrap();
    }

    #[test]
    fn the_token_sink_debug_shows_nothing_and_the_config_never_prints_a_token() {
        let (sink, _seen, _n) = recording_sink(Ok(()));
        assert_eq!(format!("{sink:?}"), "TokenSink(..)");
        let mut c = cfg_with(sink.clone());
        c.auth = Auth::Token(Secret::new("fmvt_do_not_print"));
        assert!(!format!("{c:?}").contains("fmvt_do_not_print"), "{c:?}");
    }
}
