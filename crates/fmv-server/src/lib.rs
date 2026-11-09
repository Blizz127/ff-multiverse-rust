//! Authoritative FF Multiverse server for the protocol of `fmv-protocol` (`fmv_protocol::PROTOCOL_VERSION`).
//!
//! The server hosts the areas of one `GameAdapter` (`Box<dyn GameAdapter>`, so wiring a real game
//! later is a one-line change in the binary): the *entry* area named by the configuration, which
//! every player joins, and every other area the adapter can load, which is loaded when a player
//! walks through an exit that leads there and unloaded some time after the last player left it
//! (see `world::areas`, and `fmv_protocol`, "Area travel"). It owns all positions: clients send
//! `MoveIntent`s, the server checks them against the speed rule and the adapter's `validate_move`,
//! and broadcasts the result, per area, on a fixed tick.
//!
//! It never sends asset bytes. Clients are offered the area by id, content fingerprint and
//! path-free provenance summary, and must confirm they hold the same copy locally.
//!
//! **The FIXTURE area is synthetic test content and is not evidence of FF7 support.** Wherever
//! it is surfaced (READY line, manifest, logs) it is labeled as fixture.
//!
//! Layout: `world` is the synchronous authoritative state (positions, accounts online, parties),
//! `accounts` is login and registration over the persistent store, `conn` is the per-connection
//! task (handshake, message loop, teardown), and this file owns the listener, the tick loop and
//! graceful shutdown. Entry points are [`run`] (what the binary uses) and [`Server::bind`] +
//! [`Server::serve`] (what tests use, to learn the bound port first).
//!
//! # Accounts and parties
//!
//! Every connection authenticates with a token or registers an account (`fmv_persist::Store`,
//! opened for the server's whole life; see `ServerConfig::state_dir`). A player's name, colour,
//! appearance binding and combat slot come from its stored player character
//! (`fmv_persist::PlayerCharacter`, see `character`), never from the client after registration; an
//! account that predates the record gets one at its first login without anything it has being changed.
//! Parties are one `fmv_party::PartyManager` inside the `World`; a player that
//! disconnects leaves its party. The protocol is documented in `fmv_protocol`.
//!
//! # Battles
//!
//! A server given [`BattleSettings`] (`ServerConfig::battle`) runs shared, server-authoritative
//! battles on the deterministic `fmv-ff7-battle` engine over its OWN `scene.bin` and `kernel.bin`:
//! it starts one for a whole party from a random encounter roll in an area with an encounter table,
//! or on `RequestEncounter` when `ServerConfig::debug_encounters` is on (off by default), freezes
//! the players in it, steps the engine on the world tick, validates every command and tells the
//! members what happened; clients replay it from their own files. A server without battle data
//! (the FIXTURE one) refuses every request with `BattleRejection::Unsupported`. See the `world`
//! module for the rules, and `fmv_protocol::battle` for the wire contract.
//!
//! # Character progress
//!
//! Every account has its own character progress in the game (`progress`): the character it fights as
//! (its player character's combat slot) and that character's level, experience, AP and gil, kept in the store
//! as a record of its own,
//! separate from the story state that selects its phase. A battle's `EncounterStart` carries each
//! member's own character and level, a victory pays each member that fought on, and the payment is
//! applied to the member's progress (with the game's level-up rule), told to it as a
//! `CharacterProgress` message (which it also gets on joining) and saved by the `saver` task, which
//! `serve` stops, after saving everything queued, before it returns.
//!
//! # Story, phases and instances
//!
//! Every account has one story progression per game (`fmv_persist::Progression`, an opaque blob that
//! only the game's adapter reads; an account that never advanced has none, which is a new account).
//! The adapter turns it into a story PHASE for each area (`fmv_adapter_api::AreaRuntime::phase_key`), and
//! the world keeps one INSTANCE of an area per phase somebody is in (`world::instances`): players see and
//! fight with only the players of their own instance. Solo, a player is in the instance of its own phase;
//! in a party, in the one of its leader's phase, keeping its own progression. The empty progression is the
//! empty phase in every area, so every account that exists today shares one instance of each area until
//! one of them advances the story.
//!
//! The story advances only by something the server verifies from the player's accepted moves: reaching a
//! story point of the area (`AreaRuntime::story_points`; for FF7, a field script's own "step on this
//! walkmesh triangle while the story counter is N" loop, see `fmv_ff7::story`). A step is logged as
//! `story advanced account=<n> entity=<n> area=<AreaId> point=<id> moment=<from>-><to>`, saved off the
//! world lock by the `saver` with the adapter's merge (so a stale save never takes a story back), and
//! told to the player with `ServerMsg::StoryUpdate`; a changed phase moves the player (and its party's
//! members) to another instance of the same area. See `world::story`.
//!
//! # Inventory
//!
//! Every account also carries items (`inventory`), mirrored in the world and stored inside the same
//! character record. The battle engine knows no inventory: the server checks the count before it submits
//! an item command and takes the item only when the engine accepted it, drops items after a win (rolled
//! from the battle's seed, logged, dealt to the living members) and tells the player what it carries
//! (`Inventory`) when it joins and after each change. A new account, and one whose record predates the
//! inventory, starts with the game's starting stock exactly once.
//!
//! # Limits and refusals
//!
//! The protocol is not encrypted and the server is for a network you trust, but one host must not be
//! able to hold it hostage: [`ServerConfig`] bounds the connections one address may hold
//! (`max_connections_per_ip`), the time a connection may stay silent before its `Hello`
//! (`hello_timeout`), the accounts one address and the whole server may create
//! (`registrations_per_ip`, `registrations_per_minute`, `max_accounts`), and how long a joined session
//! may idle (`idle_timeout`: it sent nothing; `inactivity_timeout`: it only pinged). Only accounts that
//! really come into being use up a registration limit (see `limits`). Every refusal is
//! `Reject { reason, code }` with a [`fmv_protocol::RejectCode`] a client acts on. A registration is
//! validated before it takes a place in the limits and before anything is written, is created in one
//! store call, and is released again if its `Welcome` (the only copy of the token) was never written
//! (see `accounts` and `conn`). Log lines a stranger can cause at will are budgeted per address, and
//! nothing is logged while the world lock is held (see `deferred`).
//!
//! # What the server trusts an adapter for
//!
//! * **FIXTURE labeling does not rest on the adapter's flag alone.** An area is announced as
//!   FIXTURE (READY line, manifest, log) when the adapter says so, belongs to the Fixture game,
//!   has a `Synthetic` provenance platform or lists a `SourceRef::Fixture` source, and its
//!   manifest display name is prefixed with `FIXTURE:` if it does not already say so. An area
//!   whose provenance names a different game than its id is refused outright.
//! * **`AreaInfo::fingerprint` is the content check a joining client faces** (besides the
//!   `AreaId`), and `JoinArea` also carries the client's own path-free provenance, which must be
//!   `compatible_with` the area's (game, platform, edition, region, revision). The fingerprint
//!   should therefore cover everything that makes two copies of an area incompatible, the
//!   provenance included and not only the area's file bytes, so that two clients on different
//!   editions with identical bytes for the hosted area cannot share an instance. Protocol v2
//!   cannot verify either claim: a client that simply echoes the public fingerprint and
//!   provenance cannot be told apart from an honest one.
//! * **`AreaRuntime::validate_move` is called once per sub-step** of every `MoveIntent` that
//!   passes the speed check (see `world`: at most `max_speed / 60` units per step, 64 steps at
//!   most), always from the previous accepted step and never with a client-supplied height. A
//!   destination-only check is therefore enough to keep clients off holes and walls between two
//!   walkable points, as long as those are wider than one step.

pub mod admin;
mod accounts;
mod character;
pub mod companion;
mod config;
mod conn;
mod deferred;
mod inventory;
pub mod ff7;
mod limits;
mod progress;
mod saver;
pub mod scene_host;
mod world;

pub use world::{SceneHost, SceneInfo, SceneReply, SceneResult, SceneSettings, TriggerInfo};

pub use accounts::MAX_APPEARANCE_REF;

pub use config::{
    BattleSettings, ConfigError, ServerConfig, DEFAULT_HELLO_TIMEOUT, DEFAULT_IDLE_TIMEOUT, DEFAULT_INACTIVITY_TIMEOUT, DEFAULT_KEEPALIVE,
    DEFAULT_MAX_CONNECTIONS_PER_IP, DEFAULT_REGISTRATIONS_PER_IP, DEFAULT_REGISTRATIONS_PER_MINUTE, MAX_BUILD_SUFFIX,
    MAX_CONNECTIONS_LIMIT, MAX_REGISTRATIONS_PER_MINUTE, MAX_TICK_HZ,
};
pub use conn::{SERVER_BUILD, SHUTDOWN_REASON};
pub use limits::PEER_REGISTRATION_WINDOW;

use std::future::Future;
use std::io::Write;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fmv_adapter_api::{AdapterError, AreaInfo, GameAdapter};
use fmv_core::{AreaId, Platform, SourceRef};
use fmv_persist::{Store, StoreOptions};
use fmv_protocol::{RejectCode, ServerMsg, PROTOCOL_VERSION};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{watch, Semaphore};
use tokio::time::MissedTickBehavior;
use tracing::{debug, info, warn, Instrument};

use crate::conn::{reject_and_close, serve_connection, wait_flag, Shared};
use crate::limits::{throttle_notice, Limits, LogVerdict, PeerKey};
use crate::world::World;

/// Pause after a failed `accept` (for example file-descriptor exhaustion) to avoid a hot loop.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);
/// At most this many "server full" farewells are written concurrently; beyond that the socket
/// is simply closed.
const MAX_REJECT_TASKS: usize = 32;

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("cannot load area {key:?}: {source} (areas offered by this adapter: {available})")]
    Area {
        // `available` is capped (see `list_available`): a real game offers hundreds of areas.
        key: String,
        available: String,
        #[source]
        source: AdapterError,
    },
    #[error("area {0} cannot be hosted: {1}")]
    BadArea(AreaId, String),
    #[error("cannot open the account store: {0}")]
    Store(#[from] fmv_persist::PersistError),
    #[error("cannot bind {addr}: {source}")]
    Bind {
        addr: SocketAddr,
        #[source]
        source: std::io::Error,
    },
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}

/// Most area keys named in the "area not found" error; the rest is summarized by a count.
const MAX_LISTED_AREAS: usize = 20;

/// The adapter's area keys for an error message, capped so a whole game (FF7 has hundreds of
/// fields) does not become one enormous line.
fn list_available(adapter: &dyn GameAdapter) -> String {
    let areas = adapter.list_areas();
    let mut text = areas.iter().take(MAX_LISTED_AREAS).map(|a| a.id.key.as_str()).collect::<Vec<_>>().join(", ");
    if areas.len() > MAX_LISTED_AREAS {
        text.push_str(&format!(", ... ({} more)", areas.len() - MAX_LISTED_AREAS));
    }
    text
}

/// How the hosted area is announced, derived from everything the adapter reports.
#[derive(Debug, PartialEq, Eq)]
struct AreaLabel {
    /// Synthetic FIXTURE content, never evidence of support for a real game. Clients must show
    /// this to the user (`AreaManifest::fixture`).
    fixture: bool,
    /// Why the area counts as FIXTURE (empty for real content), for the log.
    reasons: Vec<&'static str>,
    /// Display name for the manifest. Carries the FIXTURE label for fixture areas.
    display_name: String,
}

/// Decide how to announce `info`. The adapter's own `fixture` flag is one input, not the
/// verdict: a stricter reading of its provenance (the rule `fmv-import` applies to library sets)
/// also marks the area FIXTURE. Refuses an area whose provenance names another game than its id,
/// since one of the two is then wrong about what the content is.
fn label_area(info: &AreaInfo) -> Result<AreaLabel, ServerError> {
    if info.provenance.game != info.id.game {
        return Err(ServerError::BadArea(
            info.id.clone(),
            format!("its provenance is for {:?} but the area belongs to {:?}", info.provenance.game, info.id.game),
        ));
    }
    let mut reasons = Vec::new();
    if info.fixture {
        reasons.push("the adapter flags it as fixture");
    }
    if info.id.game.is_fixture() {
        reasons.push("it belongs to the Fixture game");
    }
    if info.provenance.platform == Platform::Synthetic {
        reasons.push("its provenance platform is Synthetic");
    }
    if info.provenance.sources.iter().any(|s| matches!(s, SourceRef::Fixture)) {
        reasons.push("its provenance lists a Fixture source");
    }
    let fixture = !reasons.is_empty();
    let display_name = if fixture && !info.display_name.to_uppercase().contains("FIXTURE") {
        format!("FIXTURE: {}", info.display_name)
    } else {
        info.display_name.clone()
    };
    Ok(AreaLabel { fixture, reasons, display_name })
}

/// A bound, not yet serving server.
pub struct Server {
    listener: TcpListener,
    local_addr: SocketAddr,
    shared: Arc<Shared>,
    fixture: bool,
}

impl Server {
    /// Load the configured area from `adapter` and bind the listener (port 0 picks a free port).
    pub async fn bind(config: ServerConfig, adapter: Box<dyn GameAdapter>) -> Result<Self, ServerError> {
        config.validate()?;
        // The adapter stays with the world: the areas behind the entry area's exits are loaded
        // from it when a player first walks there.
        let adapter: Arc<dyn GameAdapter> = Arc::from(adapter);
        let area = adapter.load_area(&config.area_key).map_err(|source| ServerError::Area {
            key: config.area_key.clone(),
            available: list_available(adapter.as_ref()),
            source,
        })?;

        // `AreaInfo::provenance` carries local install paths. Only the path-free summary may be
        // sent or logged; never format the full `AreaInfo` or `EditionProvenance`.
        let info = area.info().clone();
        let (exit_count, npc_count) = (area.exits().len(), area.npcs().len());
        let story_points: Vec<String> = area.story_points().iter().map(|p| format!("point={} {}", p.id, p.label)).collect();
        let label = label_area(&info)?;
        let fixture = label.fixture;
        if fixture && !info.fixture {
            warn!(reasons = ?label.reasons, "the adapter did not flag this area as fixture; labeling it FIXTURE anyway");
        }
        let area_id = info.id.clone();
        let fingerprint = info.fingerprint;
        let provenance = info.provenance.summary();
        let manifest = Arc::new(ServerMsg::AreaManifest {
            area: info.id.clone(),
            display_name: label.display_name.clone(),
            fingerprint,
            provenance: provenance.clone(),
            fixture,
        });
        let display_name = label.display_name;

        let mut world = World::with_party_config(area, config.party)?;
        let saver_adapter = Arc::clone(&adapter);
        world.set_area_source(adapter);
        world.set_travel_timeout(config.join_timeout);
        world.configure_battle(config.battle.clone(), config.debug_encounters);
        world.configure_scenes(config.scenes.clone());
        let (store, ephemeral_state) = open_store(&config).await?;
        // Every server saves, off the world lock (see `saver`): what battles pay (a server that runs
        // them) and the story steps players make (any server whose adapter has a story).
        let saver = saver::Saver::spawn(
            Arc::clone(&store),
            area_id.game,
            config.battle.as_ref().map(|b| Arc::clone(&b.rules)),
            saver_adapter,
        );
        world.set_progress_saver(saver.handle());
        let saver = Some(saver);
        let listener = TcpListener::bind(config.bind)
            .await
            .map_err(|source| ServerError::Bind { addr: config.bind, source })?;
        let local_addr = listener.local_addr()?;

        info!(area = %area_id, name = %display_name, ?provenance, fixture, exits = exit_count, npcs = npc_count, story_points = story_points.len(), "hosting area (the entry area; the areas its exits lead to are loaded when a player walks there)");
        for point in &story_points {
            // One line per point the server watches in this area (see `world::story`).
            info!(area = %area_id, "story point {point}");
        }
        if fixture {
            warn!("hosting a FIXTURE area: synthetic test content, NOT FF7 support");
        }
        match (&config.battle, config.debug_encounters) {
            (Some(b), debug_on) => {
                info!(
                    debug_encounters = debug_on,
                    debug_limit_full = b.debug_limit_full,
                    debug_status = %b.debug_status,
                    debug_bonus_exp = b.debug_bonus_exp,
                    debug_bonus_limit = b.debug_bonus_limit,
                    battle_speed = b.battle_speed,
                    ticks_per_second = b.ticks_per_second,
                    fixed_seed = b.fixed_seed.is_some(),
                    companion_turn_wait_ms = b.companion_pacing.turn_wait_ms,
                    companion_max_wait_ms = b.companion_pacing.max_wait_ms,
                    "battles supported (server-authoritative; clients replay them from their own battle data)"
                );
                if b.debug_limit_full {
                    warn!(
                        "DEBUG: --debug-limit-full is on: every member of every battle starts with a FULL limit gauge instead of its account's own (a smoke-test aid; leave it off on a server real players use). \
                         No limit gauge is ever stored under it: the end-of-battle gauge save is skipped (logged as `limit not kept ... reason=debug-limit-full`), so a debug run can never overwrite a real account's gauge"
                    );
                }
                if !b.debug_status.is_empty() {
                    warn!(
                        "DEBUG: --debug-status {} is on: every member of every battle starts with those statuses instead of none (a smoke-test aid; leave it off on a server real players use). \
                         Nothing is stored under it: it only changes what a battle starts with (the `statuses=[..]` key of each start line says it)",
                        b.debug_status
                    );
                }
                if b.debug_bonus_exp > 0 {
                    warn!(
                        "DEBUG: --debug-bonus-exp {} is on: every won battle pays each member that fought on {} experience MORE than it earned, through the ordinary save path. \
                         What is stored is indistinguishable from earned experience: this server must only ever run on a throw-away state directory (a smoke-test aid; leave it off on a server real players use)",
                        b.debug_bonus_exp, b.debug_bonus_exp
                    );
                }
                if b.debug_bonus_limit > 0 {
                    warn!(
                        "DEBUG: --debug-bonus-limit {} is on: the limit gauge every member is kept with after a battle is {} more than the battle left it (up to the cap), through the ordinary save path. \
                         What is stored is indistinguishable from a gauge earned in battle: this server must only ever run on a throw-away state directory (a smoke-test aid; leave it off on a server real players use)",
                        b.debug_bonus_limit, b.debug_bonus_limit
                    );
                }
            }
            (None, true) => warn!("--debug-encounters has no effect: this server has no battle data, so battles are unsupported"),
            (None, false) => info!("battles unsupported on this server (no owned battle data); battle requests are refused"),
        }
        if !local_addr.ip().is_loopback() {
            warn!(
                %local_addr,
                "listening on a non-loopback address; protocol v{} is not encrypted (account tokens and game traffic cross the network in the clear): use it only on a network you trust",
                fmv_protocol::PROTOCOL_VERSION
            );
        }

        let build = config.build_string();
        let limits = Limits::new(&config);
        let shared = Arc::new(Shared {
            cfg: config,
            world: Mutex::new(world),
            manifest,
            area_id,
            fingerprint,
            provenance: provenance.clone(),
            build,
            store,
            saver,
            limits,
            _ephemeral_state: ephemeral_state,
        });
        Ok(Self { listener, local_addr, shared, fixture })
    }

    /// The address actually bound (useful with port 0).
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn area_id(&self) -> &AreaId {
        &self.shared.area_id
    }

    /// Whether the hosted area is synthetic FIXTURE content.
    pub fn is_fixture(&self) -> bool {
        self.fixture
    }

    /// `READY protocol=<v> build=<version> addr=<bound addr> area=<AreaId> fixture=<bool>`
    pub fn ready_line(&self) -> String {
        format!(
            "READY protocol={} build={} addr={} area={} fixture={}",
            PROTOCOL_VERSION, self.shared.build, self.local_addr, self.shared.area_id, self.fixture
        )
    }

    /// Print the READY line (and nothing else) to stdout and flush. All logging goes to stderr,
    /// including a hint for a person at the terminal on how to connect.
    pub fn announce_ready(&self) {
        let mut out = std::io::stdout().lock();
        if let Err(e) = writeln!(out, "{}", self.ready_line()).and_then(|()| out.flush()) {
            warn!(error = %e, "could not write the READY line to stdout");
        }
        let game = self.shared.area_id.game.key().to_ascii_lowercase();
        info!(
            "ready on {}: connect a client with `fmv-client --server {} --game {game} --name <your name>`",
            self.local_addr, self.local_addr
        );
    }

    /// Accept and serve clients until `shutdown` resolves, then stop accepting, send every client
    /// `Reject { reason: "server shutting down" }`, wait (bounded by `shutdown_grace`) for the
    /// connections to close and return.
    pub async fn serve<F: Future<Output = ()>>(self, shutdown: F) -> Result<(), ServerError> {
        let Server { listener, shared, .. } = self;
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let slots = Arc::new(Semaphore::new(shared.cfg.max_connections));
        let reject_slots = Arc::new(Semaphore::new(MAX_REJECT_TASKS));
        let tick = tokio::spawn(tick_loop(Arc::clone(&shared), shutdown_rx.clone()));

        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                () = &mut shutdown => break,
                accepted = listener.accept() => {
                    let (stream, peer) = match accepted {
                        Ok(ok) => ok,
                        Err(e) => {
                            // Not routine (file-descriptor exhaustion), but it repeats every
                            // `ACCEPT_BACKOFF` for as long as it lasts: throttled like the rest.
                            match shared.limits.logs.allow(None, Instant::now()) {
                                LogVerdict::Emit { .. } => warn!(error = %e, "accept failed"),
                                LogVerdict::FirstSuppressed => {
                                    throttle_notice();
                                    debug!(error = %e, "accept failed (log throttled)");
                                }
                                LogVerdict::Suppress => debug!(error = %e, "accept failed (log throttled)"),
                            }
                            tokio::time::sleep(ACCEPT_BACKOFF).await;
                            continue;
                        }
                    };
                    let key = PeerKey::of(peer.ip());
                    // One address holds at most its share of the connection slots (handshaking
                    // ones included): checked first, so a flood from one host is refused as that
                    // host's and never even reaches the global count.
                    let peer_permit = match shared.limits.connections.admit(key) {
                        Ok(permit) => permit,
                        Err(_) => {
                            let max = shared.cfg.max_connections_per_ip.unwrap_or(0);
                            refuse(
                                &shared,
                                &reject_slots,
                                stream,
                                peer,
                                RejectCode::TooManyConnections,
                                format!("too many connections from your address (at most {max} at once)"),
                            );
                            continue;
                        }
                    };
                    match Arc::clone(&slots).try_acquire_owned() {
                        Ok(permit) => {
                            let shared = Arc::clone(&shared);
                            let shutdown_rx = shutdown_rx.clone();
                            tokio::spawn(
                                async move {
                                    // Both places are given back together, when the connection (its
                                    // farewell and linger included) is over.
                                    let _permit = permit;
                                    let _peer_permit = peer_permit;
                                    serve_connection(shared, stream, peer, shutdown_rx).await;
                                }
                                .instrument(tracing::info_span!("conn", %peer)),
                            );
                        }
                        Err(_) => {
                            drop(peer_permit);
                            refuse(&shared, &reject_slots, stream, peer, RejectCode::ServerFull, "server full".to_string());
                        }
                    }
                }
            }
        }

        info!("shutdown requested; no longer accepting connections");
        drop(listener);
        shutdown_tx.send_replace(true);
        // Every connection task holds one permit, so owning all of them means all are done.
        let all = u32::try_from(shared.cfg.max_connections).unwrap_or(u32::MAX);
        match tokio::time::timeout(shared.cfg.shutdown_grace, slots.acquire_many(all)).await {
            Ok(_) => info!("all connections closed"),
            Err(_) => warn!(grace = ?shared.cfg.shutdown_grace, "grace period elapsed with connections still open"),
        }
        let _ = tick.await;
        // Every connection is closed and the world no longer ticks: nothing more can be paid. Save what
        // is queued and wait for the saver to let go of the store, so that whoever opens this state
        // directory next finds everything in it and its lock free.
        if let Some(saver) = &shared.saver {
            saver.shutdown(shared.cfg.shutdown_grace).await;
        }
        Ok(())
    }
}

/// Turn away a connection that was accepted but gets no slot (the server is full, or its address
/// holds its share): say why while a farewell task is free (at most `MAX_REJECT_TASKS` at once;
/// past that the socket is simply closed) and log it within the log throttle's budget, so a flood of
/// refused connections is not a flood of log lines.
fn refuse(shared: &Arc<Shared>, reject_slots: &Arc<Semaphore>, stream: TcpStream, peer: SocketAddr, code: RejectCode, text: String) {
    match shared.limits.logs.allow(Some(PeerKey::of(peer.ip())), Instant::now()) {
        LogVerdict::Emit { suppressed: 0 } => warn!(%peer, ?code, reason = %text, "connection refused"),
        LogVerdict::Emit { suppressed } => {
            warn!(%peer, ?code, suppressed, reason = %text, "connection refused (earlier refusals were throttled; see debug)")
        }
        LogVerdict::FirstSuppressed => {
            throttle_notice();
            debug!(%peer, ?code, reason = %text, "connection refused (log throttled)");
        }
        LogVerdict::Suppress => debug!(%peer, ?code, reason = %text, "connection refused (log throttled)"),
    }
    match Arc::clone(reject_slots).try_acquire_owned() {
        Ok(reject_permit) => {
            let wt = shared.cfg.write_timeout.min(Duration::from_secs(2));
            tokio::spawn(async move {
                let _reject_permit = reject_permit;
                reject_and_close(stream, code, &text, wt).await;
            });
        }
        // Too many farewells already in flight: just hang up.
        Err(_) => drop(stream),
    }
}

/// Bind, announce READY on stdout and serve until `shutdown` resolves.
pub async fn run<F: Future<Output = ()>>(
    config: ServerConfig,
    adapter: Box<dyn GameAdapter>,
    shutdown: F,
) -> Result<(), ServerError> {
    let server = Server::bind(config, adapter).await?;
    server.announce_ready();
    server.serve(shutdown).await
}

/// Open the account store: `config.state_dir`, or a private temporary directory that lives as long
/// as the server when none is configured. Opening takes the store's single-writer lock and may
/// sweep leftovers of a crashed run; what it noticed is logged. Blocking disk I/O, so it runs off
/// the async executor.
async fn open_store(config: &ServerConfig) -> Result<(Arc<Store>, Option<tempfile::TempDir>), ServerError> {
    let (root, ephemeral) = match &config.state_dir {
        Some(dir) => (dir.clone(), None),
        None => {
            let dir = tempfile::Builder::new().prefix("fmv-server-state-").tempdir()?;
            (dir.path().to_path_buf(), Some(dir))
        }
    };
    let opened = {
        let root = root.clone();
        let options = StoreOptions::default().with_max_accounts(config.max_accounts);
        tokio::task::spawn_blocking(move || Store::open_with(root, options))
            .await
            .map_err(|e| ServerError::Io(std::io::Error::other(format!("store open task failed: {e}"))))?
    };
    let store = opened?;
    for warning in store.open_warnings() {
        warn!(%warning, "account store");
    }
    info!(
        state = %root.display(),
        ephemeral = ephemeral.is_some(),
        registration = config.allow_registration,
        accounts = store.account_count(),
        max_accounts = store.max_accounts(),
        "account store open"
    );
    Ok((Arc::new(store), ephemeral))
}

/// Advance the world once per tick; each tick broadcasts the entities that changed (if any),
/// expires timed-out party invites and steps the running battles.
async fn tick_loop(shared: Arc<Shared>, mut shutdown: watch::Receiver<bool>) {
    // `validate()` guarantees tick_hz >= 1, so the period is never zero.
    let period = Duration::from_secs_f64(1.0 / f64::from(shared.cfg.tick_hz));
    let mut interval = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = interval.tick() => {
                let now = Instant::now();
                shared.with_world(|world| {
                    world.advance_tick();
                    world.expire_parties(now);
                    world.advance_battles(now);
                    world.advance_scenes(now);
                    world.manage_areas(now);
                });
            }
            () = wait_flag(&mut shutdown) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_adapter_api::{AreaRuntime, Bounds, MoveResult};
    use fmv_core::{EditionProvenance, Fingerprint, GameId, Pos};
    use fmv_fixture::FixtureAdapter;

    /// What a real FF7 PC area would report (the install path stays local).
    fn real_info() -> AreaInfo {
        AreaInfo {
            id: AreaId { game: GameId::Ff7, key: "md1stin".into() },
            display_name: "FF7 field md1stin".into(),
            fingerprint: Fingerprint([9; 32]),
            provenance: EditionProvenance {
                game: GameId::Ff7,
                platform: Platform::Pc,
                edition: "PC 1998".into(),
                region: "NTSC-U".into(),
                language: "en".into(),
                revision: "1.02".into(),
                sources: vec![SourceRef::Install { path: "/home/owner/private/ff7".into() }],
                modded: false,
            },
            fixture: false,
        }
    }

    struct StubArea(AreaInfo);

    impl AreaRuntime for StubArea {
        fn info(&self) -> &AreaInfo {
            &self.0
        }
        fn spawn_points(&self) -> Vec<Pos> {
            vec![Pos::default()]
        }
        fn validate_move(&self, _from: Pos, to: Pos) -> MoveResult {
            MoveResult::Accepted(to)
        }
        fn bounds(&self) -> Bounds {
            Bounds { min: Pos::default(), max: Pos::default() }
        }
        fn walk_triangles(&self) -> Vec<[Pos; 3]> {
            Vec::new()
        }
        fn max_speed(&self) -> f32 {
            100.0
        }
    }

    struct StubAdapter(AreaInfo);

    impl GameAdapter for StubAdapter {
        fn game(&self) -> GameId {
            self.0.id.game
        }
        fn list_areas(&self) -> Vec<AreaInfo> {
            vec![self.0.clone()]
        }
        fn load_area(&self, key: &str) -> Result<Box<dyn AreaRuntime>, AdapterError> {
            if key == self.0.id.key {
                Ok(Box::new(StubArea(self.0.clone())))
            } else {
                Err(AdapterError::AreaNotFound(key.to_string()))
            }
        }
    }

    async fn bind(info: AreaInfo) -> Result<Server, ServerError> {
        let config = ServerConfig { bind: "127.0.0.1:0".parse().unwrap(), area_key: info.id.key.clone(), ..ServerConfig::default() };
        Server::bind(config, Box::new(StubAdapter(info))).await
    }

    /// An adapter that offers `n` areas (`a0`, `a1`, ...), like a whole game does.
    struct ManyAreas(usize);

    impl GameAdapter for ManyAreas {
        fn game(&self) -> GameId {
            GameId::Ff7
        }
        fn list_areas(&self) -> Vec<AreaInfo> {
            (0..self.0)
                .map(|i| {
                    let mut info = real_info();
                    info.id.key = format!("a{i}");
                    info
                })
                .collect()
        }
        fn load_area(&self, key: &str) -> Result<Box<dyn AreaRuntime>, AdapterError> {
            Err(AdapterError::AreaNotFound(key.to_string()))
        }
    }

    #[test]
    fn the_list_of_available_areas_in_an_error_is_capped() {
        assert_eq!(list_available(&ManyAreas(3)), "a0, a1, a2");
        assert_eq!(list_available(&ManyAreas(0)), "");
        let exactly = list_available(&ManyAreas(MAX_LISTED_AREAS));
        assert!(!exactly.contains("more"), "{exactly}");
        let many = list_available(&ManyAreas(MAX_LISTED_AREAS + 7));
        assert!(many.starts_with("a0, a1, a2") && many.contains("a19") && !many.contains("a20,"), "{many}");
        assert!(many.ends_with("... (7 more)"), "{many}");
        assert!(many.len() < 200, "a whole game must not become one enormous line: {} bytes", many.len());
    }

    #[test]
    fn real_content_is_not_labeled_fixture() {
        let label = label_area(&real_info()).unwrap();
        assert_eq!(label, AreaLabel { fixture: false, reasons: vec![], display_name: "FF7 field md1stin".into() });
    }

    #[test]
    fn the_fixture_adapters_own_area_is_fixture_and_keeps_its_name() {
        let info = FixtureAdapter::new().list_areas().remove(0);
        let label = label_area(&info).unwrap();
        assert!(label.fixture);
        assert_eq!(label.display_name, info.display_name, "already carries the FIXTURE label");
        assert!(label.display_name.contains("FIXTURE"));
    }

    #[test]
    fn fixture_is_derived_from_provenance_not_only_from_the_adapters_flag() {
        // Each of these claims to be a real game and says `fixture: false`.
        let synthetic = {
            let mut i = real_info();
            i.provenance.platform = Platform::Synthetic;
            i
        };
        let fixture_source = {
            let mut i = real_info();
            i.provenance.sources.push(SourceRef::Fixture);
            i
        };
        let flagged_only = {
            let mut i = real_info();
            i.fixture = true;
            i
        };
        for (info, why) in [(synthetic, "Synthetic"), (fixture_source, "Fixture source"), (flagged_only, "adapter flags")] {
            let label = label_area(&info).unwrap();
            assert!(label.fixture, "{why}");
            assert!(label.reasons.iter().any(|r| r.contains(why)), "{why}: {:?}", label.reasons);
            assert_eq!(label.display_name, "FIXTURE: FF7 field md1stin", "{why}: the manifest name must carry the label");
        }
    }

    #[test]
    fn an_area_whose_provenance_names_another_game_is_refused() {
        let mut info = real_info();
        info.provenance.game = GameId::Fixture;
        let err = label_area(&info).unwrap_err();
        assert!(matches!(err, ServerError::BadArea(..)), "{err}");
        assert!(err.to_string().contains("provenance"), "{err}");

        let mut info = FixtureAdapter::new().list_areas().remove(0);
        info.provenance.game = GameId::Ff7; // a fixture id dressed up with real provenance
        assert!(label_area(&info).is_err());
    }

    #[tokio::test]
    async fn bind_announces_the_derived_label_on_the_ready_line_and_in_the_manifest() {
        // Real-looking area: not fixture, name untouched, no local path anywhere in the manifest.
        let server = bind(real_info()).await.unwrap();
        assert!(!server.is_fixture());
        assert!(server.ready_line().ends_with("area=Ff7/md1stin fixture=false"), "{}", server.ready_line());
        match &*server.shared.manifest {
            ServerMsg::AreaManifest { fixture, display_name, provenance, .. } => {
                assert!(!fixture);
                assert_eq!(display_name, "FF7 field md1stin");
                assert!(!format!("{provenance:?}").contains("private"), "no install path may reach the wire");
            }
            other => panic!("{other:?}"),
        }

        // Same area, but its provenance is synthetic while the adapter says `fixture: false`.
        let mut sneaky = real_info();
        sneaky.provenance.platform = Platform::Synthetic;
        let server = bind(sneaky).await.unwrap();
        assert!(server.is_fixture());
        assert!(server.ready_line().ends_with("area=Ff7/md1stin fixture=true"), "{}", server.ready_line());
        match &*server.shared.manifest {
            ServerMsg::AreaManifest { fixture, display_name, .. } => {
                assert!(fixture);
                assert!(display_name.starts_with("FIXTURE"), "{display_name}");
            }
            other => panic!("{other:?}"),
        }

        // Contradictory provenance never starts a listener.
        let mut wrong = real_info();
        wrong.provenance.game = GameId::Ff8;
        match bind(wrong).await {
            Err(ServerError::BadArea(id, why)) => {
                assert_eq!(id.key, "md1stin");
                assert!(why.contains("Ff8"), "{why}");
            }
            Err(other) => panic!("wrong error: {other}"),
            Ok(_) => panic!("must not bind"),
        }
    }
}
