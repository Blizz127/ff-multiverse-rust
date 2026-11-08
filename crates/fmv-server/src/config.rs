//! Server configuration and its validation.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use fmv_fixture::FIXTURE_AREA_KEY;
use fmv_protocol::DEFAULT_DEV_PORT;

/// Highest accepted tick rate (`--tick-hz`).
pub const MAX_TICK_HZ: u32 = 240;

/// Highest accepted connection cap (`--max-connections`).
///
/// Protocol v1 sends the whole area snapshot in a single frame (`MAX_FRAME` = 256 KiB).
/// Display names (24 chars) and appearance refs (64 chars) are bounded, so one entity
/// serializes to at most about 550 bytes of JSON (measured with 4-byte characters everywhere,
/// 20-digit ids and extreme floats; see the `worst_case_snapshot_fits_in_one_frame` test) and 256
/// players (about 140 KB, a little over half of `MAX_FRAME`) still fit with room to spare.
pub const MAX_CONNECTIONS_LIMIT: usize = 256;

/// Bounds for `ServerConfig::outbound_queue` (messages per connection).
pub const MIN_OUTBOUND_QUEUE: usize = 8;
pub const MAX_OUTBOUND_QUEUE: usize = 65_536;

/// Default for `ServerConfig::idle_timeout`.
///
/// The reference client sends a `Ping` every second, so 30 s of silence from a joined client
/// means the peer is gone (or is holding a slot for nothing), not that it is standing still.
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Default for `ServerConfig::keepalive` (idle time before the first TCP keepalive probe).
pub const DEFAULT_KEEPALIVE: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
#[error("invalid server configuration: {0}")]
pub struct ConfigError(pub String);

/// Runtime configuration of one server instance (one hosted area).
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Listen address. Port 0 picks a free port; see `Server::local_addr`.
    pub bind: SocketAddr,
    /// Key of the area to host, as understood by `GameAdapter::load_area`.
    pub area_key: String,
    /// Broadcast ticks per second (`1..=MAX_TICK_HZ`).
    pub tick_hz: u32,
    /// Maximum simultaneous connections, handshaking ones included.
    pub max_connections: usize,
    /// A new connection must send `Hello` within this time.
    pub hello_timeout: Duration,
    /// A greeted connection must answer the area manifest (`JoinArea`/`CannotJoin`) within
    /// this total time.
    pub join_timeout: Duration,
    /// One outbound frame must be fully written within this time or the client is dropped.
    pub write_timeout: Duration,
    /// Drop joined clients that send no message for this long (`Reject { "idle timeout ..." }`).
    /// A joined client must therefore send something, for example a `Ping`, well inside this
    /// window; the reference client pings every second. This is what frees the slot and the
    /// avatar of a silent socket or of a peer that vanished without a FIN/RST. Defaults to
    /// [`DEFAULT_IDLE_TIMEOUT`]; `None` disables it (then only `keepalive` and TCP's own
    /// retransmission give-up eventually evict a dead peer).
    pub idle_timeout: Option<Duration>,
    /// Enable TCP keepalive on accepted sockets, probing after this much idle time (then every
    /// 10 s, giving up after 3 failed probes). A second line of defence against half-open
    /// connections that also works while `idle_timeout` is disabled. `None` leaves the OS
    /// default (keepalive off).
    pub keepalive: Option<Duration>,
    /// Sustained inbound message rate allowed per connection; the burst allowance is one
    /// second's worth. Exceeding it disconnects the client.
    pub max_msgs_per_sec: u32,
    /// Capacity of each connection's bounded outbound queue. A client whose queue fills up
    /// is disconnected so it can never stall the others.
    pub outbound_queue: usize,
    /// On shutdown, how long to wait for clients to flush their farewell and close.
    pub shutdown_grace: Duration,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), DEFAULT_DEV_PORT),
            area_key: FIXTURE_AREA_KEY.to_string(),
            tick_hz: 20,
            max_connections: 64,
            hello_timeout: Duration::from_secs(10),
            join_timeout: Duration::from_secs(30),
            write_timeout: Duration::from_secs(10),
            idle_timeout: Some(DEFAULT_IDLE_TIMEOUT),
            keepalive: Some(DEFAULT_KEEPALIVE),
            max_msgs_per_sec: 120,
            outbound_queue: 256,
            shutdown_grace: Duration::from_secs(5),
        }
    }
}

impl ServerConfig {
    /// Checks every field that would otherwise panic or misbehave later
    /// (for example a zero tick rate would panic when building the tick interval).
    pub fn validate(&self) -> Result<(), ConfigError> {
        let bad = |msg: String| Err(ConfigError(msg));
        if !(1..=MAX_TICK_HZ).contains(&self.tick_hz) {
            return bad(format!("tick_hz must be 1..={MAX_TICK_HZ} (got {})", self.tick_hz));
        }
        if !(1..=MAX_CONNECTIONS_LIMIT).contains(&self.max_connections) {
            return bad(format!(
                "max_connections must be 1..={MAX_CONNECTIONS_LIMIT} (got {})",
                self.max_connections
            ));
        }
        if self.max_msgs_per_sec == 0 {
            return bad("max_msgs_per_sec must be at least 1".into());
        }
        if !(MIN_OUTBOUND_QUEUE..=MAX_OUTBOUND_QUEUE).contains(&self.outbound_queue) {
            return bad(format!(
                "outbound_queue must be {MIN_OUTBOUND_QUEUE}..={MAX_OUTBOUND_QUEUE} (got {})",
                self.outbound_queue
            ));
        }
        for (name, d) in [
            ("hello_timeout", self.hello_timeout),
            ("join_timeout", self.join_timeout),
            ("write_timeout", self.write_timeout),
            ("shutdown_grace", self.shutdown_grace),
        ] {
            if d.is_zero() {
                return bad(format!("{name} must be non-zero"));
            }
        }
        if self.idle_timeout.is_some_and(|d| d.is_zero()) {
            return bad("idle_timeout must be non-zero (use None to disable)".into());
        }
        // The kernel takes whole seconds and rejects 0, so anything under a second is invalid.
        if self.keepalive.is_some_and(|d| d < Duration::from_secs(1)) {
            return bad("keepalive must be at least one second (use None to disable)".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_core::{AnimState, AreaId, AvatarAppearance, EntityId, EntitySnapshot, GameId, Pos};
    use fmv_protocol::{encode, ServerMsg, MAX_FRAME};

    #[test]
    fn default_is_valid_and_matches_spec_defaults() {
        let c = ServerConfig::default();
        c.validate().unwrap();
        assert_eq!(c.bind.port(), DEFAULT_DEV_PORT);
        assert!(c.bind.ip().is_loopback());
        assert_eq!(c.tick_hz, 20);
        assert_eq!(c.max_connections, 64);
        assert_eq!(c.area_key, FIXTURE_AREA_KEY);
        assert_eq!(c.hello_timeout, Duration::from_secs(10));
    }

    #[test]
    fn silent_and_vanished_peers_are_evicted_by_default() {
        // The reference client pings every second, so a joined client that has been silent for
        // the default window is gone. Without this a silent socket (or a half-open one) would
        // hold a connection slot and its avatar until the server restarts.
        let c = ServerConfig::default();
        assert_eq!(c.idle_timeout, Some(DEFAULT_IDLE_TIMEOUT));
        assert!(DEFAULT_IDLE_TIMEOUT >= Duration::from_secs(10), "must stay well above the 1 s client ping");
        assert!(DEFAULT_IDLE_TIMEOUT <= Duration::from_secs(60), "a dead peer should not linger for minutes");
        assert_eq!(c.keepalive, Some(DEFAULT_KEEPALIVE));
    }

    #[test]
    fn rejects_values_that_would_panic_or_misbehave() {
        let ok = ServerConfig::default();
        for c in [
            ServerConfig { tick_hz: 0, ..ok.clone() },
            ServerConfig { tick_hz: MAX_TICK_HZ + 1, ..ok.clone() },
            ServerConfig { max_connections: 0, ..ok.clone() },
            ServerConfig { max_connections: MAX_CONNECTIONS_LIMIT + 1, ..ok.clone() },
            ServerConfig { max_msgs_per_sec: 0, ..ok.clone() },
            ServerConfig { outbound_queue: MIN_OUTBOUND_QUEUE - 1, ..ok.clone() },
            ServerConfig { outbound_queue: MAX_OUTBOUND_QUEUE + 1, ..ok.clone() },
            ServerConfig { outbound_queue: usize::MAX, ..ok.clone() },
            ServerConfig { hello_timeout: Duration::ZERO, ..ok.clone() },
            ServerConfig { idle_timeout: Some(Duration::ZERO), ..ok.clone() },
            ServerConfig { keepalive: Some(Duration::ZERO), ..ok.clone() },
            ServerConfig { keepalive: Some(Duration::from_millis(900)), ..ok.clone() },
        ] {
            assert!(c.validate().is_err(), "{c:?} should be rejected");
        }
        ServerConfig { tick_hz: MAX_TICK_HZ, max_connections: MAX_CONNECTIONS_LIMIT, ..ok.clone() }
            .validate()
            .unwrap();
        // Both protections can be switched off explicitly.
        ServerConfig { idle_timeout: None, keepalive: None, ..ok }.validate().unwrap();
    }

    /// The `MAX_CONNECTIONS_LIMIT` claim: the biggest snapshot the server can ever build (every
    /// player at the 4-byte-character worst case, extreme floats) is one frame, because protocol
    /// v1 cannot split an `AreaSnapshot`. If this fails after a protocol change, every joiner of
    /// a full server would be dropped right after `JoinArea`.
    #[test]
    fn worst_case_snapshot_fits_in_one_frame() {
        let wide = |n: usize| "\u{1D538}".repeat(n); // 4 bytes in UTF-8, not escaped by JSON
        let entity = |i: u64| EntitySnapshot {
            id: EntityId(u64::MAX - i),
            pos: Pos::new(-f32::MAX, f32::MIN_POSITIVE, -f32::MAX),
            facing: -f32::MAX,
            anim: AnimState::Run,
            avatar: AvatarAppearance {
                display_name: wide(AvatarAppearance::MAX_NAME),
                tint: [255, 255, 255],
                appearance_ref: Some(wide(crate::MAX_APPEARANCE_REF)),
            },
        };
        let snapshot = |n: usize| ServerMsg::AreaSnapshot {
            area: AreaId { game: GameId::Fixture, key: "x".repeat(64) },
            tick: u64::MAX,
            entities: (0..n as u64).map(entity).collect(),
        };
        let len = |n: usize| encode(&snapshot(n)).expect("a snapshot of a full server must fit in one frame").len();
        let per_entity = len(2) - len(1);
        let full = len(MAX_CONNECTIONS_LIMIT);
        eprintln!("worst case: {per_entity} bytes per entity, {full} bytes for {MAX_CONNECTIONS_LIMIT} (frame limit {MAX_FRAME})");
        assert!(full <= MAX_FRAME as usize);
        // Keep real headroom: the per-entity cost may grow a little before the cap is a problem.
        assert!(full <= MAX_FRAME as usize * 3 / 4, "{full} bytes for {MAX_CONNECTIONS_LIMIT} entities ({per_entity} each)");
        assert!(per_entity <= 700, "one worst-case entity takes {per_entity} bytes; update the MAX_CONNECTIONS_LIMIT docs");
    }
}
