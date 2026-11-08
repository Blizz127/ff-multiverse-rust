//! Server configuration and its validation.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use fmv_ff7_battle::rules::{DEFAULT_BATTLE_SPEED, DEFAULT_TICKS_PER_SECOND};
use fmv_fixture::FIXTURE_AREA_KEY;
use fmv_party::PartyConfig;
use fmv_protocol::{BattleRules, DEFAULT_DEV_PORT};

/// Highest accepted tick rate (`--tick-hz`).
pub const MAX_TICK_HZ: u32 = 240;

/// Highest accepted connection cap (`--max-connections`).
///
/// Protocol v2 sends the whole area snapshot in a single frame (`MAX_FRAME` = 256 KiB).
/// Display names (24 chars) and appearance refs (64 chars) are bounded, so one entity
/// serializes to at most about 550 bytes of JSON (measured with 4-byte characters everywhere,
/// 20-digit ids and extreme floats; see the `worst_case_snapshot_fits_in_one_frame` test) and 256
/// players (about 140 KB, a little over half of `MAX_FRAME`) still fit with room to spare.
pub const MAX_CONNECTIONS_LIMIT: usize = 256;

/// Bounds for `ServerConfig::outbound_queue` (messages per connection).
pub const MIN_OUTBOUND_QUEUE: usize = 8;
pub const MAX_OUTBOUND_QUEUE: usize = 65_536;

/// Highest accepted `ServerConfig::registrations_per_minute`.
pub const MAX_REGISTRATIONS_PER_MINUTE: u32 = 10_000;

/// Default for `ServerConfig::registrations_per_minute`.
pub const DEFAULT_REGISTRATIONS_PER_MINUTE: u32 = 120;

/// Default for `ServerConfig::idle_timeout`.
///
/// The reference client sends a `Ping` every second, so 30 s of silence from a joined client
/// means the peer is gone (or is holding a slot for nothing), not that it is standing still.
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Default for `ServerConfig::keepalive` (idle time before the first TCP keepalive probe).
pub const DEFAULT_KEEPALIVE: Duration = Duration::from_secs(30);

/// How a server that can run battles runs them. `None` in [`ServerConfig::battle`] (the FIXTURE
/// server, or an FF7 server without owned battle data) means battles are unsupported and every
/// request is refused with `BattleRejection::Unsupported`.
#[derive(Clone)]
pub struct BattleSettings {
    /// The battle rules over THIS server's own game data (see `fmv_protocol::BattleRules`).
    pub rules: Arc<dyn BattleRules>,
    /// The level every battle member fights at; `None` is each character's starting level from
    /// the game data.
    pub level: Option<u8>,
    /// FF7 "Battle Speed" (0 fastest, 255 slowest), announced to clients in `EncounterStart`.
    pub battle_speed: u8,
    /// Engine ticks per second of battle time (`1..=1000`), announced in `EncounterStart`.
    pub ticks_per_second: u32,
    /// Use this seed for every battle instead of a fresh one (reproducible smokes and tests).
    pub fixed_seed: Option<u64>,
    /// The formation a debug `RequestEncounter` uses when it names none and the area has no
    /// encounter table; `None` is `rules.default_formation()`.
    pub debug_formation: Option<u16>,
    /// Random encounters: roll the area's encounter table as players walk (only does anything in
    /// an area that has a table). Off with `--no-random-encounters`.
    pub random_encounters: bool,
    /// Horizontal distance a player walks between two random-encounter rolls. `None` is the
    /// area's `max_speed`: one roll per second of walking at full speed.
    pub encounter_step_units: Option<f32>,
}

impl BattleSettings {
    /// Settings with the engine's own defaults (FF7 default battle speed, 60 ticks per second).
    pub fn new(rules: Arc<dyn BattleRules>) -> Self {
        Self {
            rules,
            level: None,
            battle_speed: DEFAULT_BATTLE_SPEED,
            ticks_per_second: DEFAULT_TICKS_PER_SECOND,
            fixed_seed: None,
            debug_formation: None,
            random_encounters: true,
            encounter_step_units: None,
        }
    }
}

impl fmt::Debug for BattleSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The rules hold the owner's game data: show only what identifies them.
        f.debug_struct("BattleSettings")
            .field("data_fingerprint", &self.rules.data_fingerprint())
            .field("level", &self.level)
            .field("battle_speed", &self.battle_speed)
            .field("ticks_per_second", &self.ticks_per_second)
            .field("fixed_seed", &self.fixed_seed)
            .field("debug_formation", &self.debug_formation)
            .field("random_encounters", &self.random_encounters)
            .field("encounter_step_units", &self.encounter_step_units)
            .finish()
    }
}

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
    /// Directory of the account store (`fmv_persist::Store`: accounts, avatar profiles,
    /// progression), which the server opens for its whole life and holds locked (a second server
    /// on the same directory is refused). `None` gives the server a private temporary directory
    /// that is deleted when it stops: accounts then last only as long as the process (tests, a
    /// quick local run). The binary always passes a real directory unless `--ephemeral` is used.
    pub state_dir: Option<PathBuf>,
    /// Whether `Auth::Register` creates accounts. When false only tokens already issued work.
    pub allow_registration: bool,
    /// Accounts the whole server creates per minute at most (a sliding window over all
    /// connections), so a flood of connections cannot fill the store with accounts.
    pub registrations_per_minute: u32,
    /// Party rules. The party clock is milliseconds of server monotonic time, so
    /// `party.invite_ttl()` is in milliseconds; the default is 3 members and 60 s.
    pub party: PartyConfig,
    /// Identity of the game adapter, appended to the build string as `<version>+<suffix>`
    /// (`Welcome::server_build` and the READY line). A real-game adapter names itself here so that
    /// two servers with different content-fingerprint rules are told apart by their build instead
    /// of being mistaken for two players with different media. One token: see
    /// [`ServerConfig::validate`]. `None` (the FIXTURE default) leaves the build string unchanged.
    pub build_suffix: Option<String>,
    /// Battle support (see [`BattleSettings`]). `None`: every battle request is refused as
    /// unsupported (the FIXTURE default).
    pub battle: Option<BattleSettings>,
    /// Let players start a battle on request (`ClientMsg::RequestEncounter`). Off by default: a
    /// normal server only starts battles from random encounters. Needs [`ServerConfig::battle`].
    pub debug_encounters: bool,
}

/// Longest accepted `ServerConfig::build_suffix` (bytes).
pub const MAX_BUILD_SUFFIX: usize = 96;

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
            state_dir: None,
            allow_registration: true,
            registrations_per_minute: DEFAULT_REGISTRATIONS_PER_MINUTE,
            party: PartyConfig::default(),
            build_suffix: None,
            battle: None,
            debug_encounters: false,
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
        if !(1..=MAX_REGISTRATIONS_PER_MINUTE).contains(&self.registrations_per_minute) {
            return bad(format!(
                "registrations_per_minute must be 1..={MAX_REGISTRATIONS_PER_MINUTE} (got {})",
                self.registrations_per_minute
            ));
        }
        if let Some(b) = &self.battle {
            if !(1..=1000).contains(&b.ticks_per_second) {
                return bad(format!("battle ticks_per_second must be 1..=1000 (got {})", b.ticks_per_second));
            }
            if b.level.is_some_and(|l| !(1..=99).contains(&l)) {
                return bad(format!("battle level must be 1..=99 (got {:?})", b.level));
            }
            if b.encounter_step_units.is_some_and(|u| !(u.is_finite() && u > 0.0)) {
                return bad(format!("encounter_step_units must be finite and positive (got {:?})", b.encounter_step_units));
            }
        }
        // The build string is one whitespace-free token on the line-based READY message.
        if let Some(suffix) = &self.build_suffix {
            let token = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'/' | b'+' | b'-');
            if suffix.is_empty() || suffix.len() > MAX_BUILD_SUFFIX || !suffix.bytes().all(token) {
                return bad(format!(
                    "build_suffix must be 1..={MAX_BUILD_SUFFIX} characters from [A-Za-z0-9._/+-] (got {suffix:?})"
                ));
            }
        }
        Ok(())
    }

    /// The build string reported in `Welcome` and on the READY line: the crate version, plus
    /// `+<build_suffix>` when the configuration names the adapter.
    pub fn build_string(&self) -> String {
        match &self.build_suffix {
            Some(suffix) => format!("{}+{suffix}", crate::SERVER_BUILD),
            None => crate::SERVER_BUILD.to_string(),
        }
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
        // FIXTURE keeps the plain version as its build string.
        assert_eq!(c.build_suffix, None);
        assert_eq!(c.build_string(), crate::SERVER_BUILD);
        // Accounts: an ephemeral store, open registration, a three-member party with 60 s invites
        // (the party clock is in milliseconds).
        assert_eq!(c.state_dir, None);
        assert!(c.allow_registration);
        assert_eq!(c.registrations_per_minute, DEFAULT_REGISTRATIONS_PER_MINUTE);
        assert_eq!((c.party.max_size(), c.party.invite_ttl()), (3, 60_000));
        // Battles: unsupported without battle data, and debug encounters are off.
        assert!(c.battle.is_none());
        assert!(!c.debug_encounters);
    }

    #[test]
    fn battle_settings_are_validated_and_never_print_their_data() {
        use fmv_protocol::SyntheticRules;
        let with = |f: &dyn Fn(&mut BattleSettings)| {
            let mut b = BattleSettings::new(Arc::new(SyntheticRules::default()));
            f(&mut b);
            ServerConfig { battle: Some(b), ..ServerConfig::default() }
        };
        with(&|_| {}).validate().unwrap();
        with(&|b| b.ticks_per_second = 1000).validate().unwrap();
        with(&|b| b.level = Some(99)).validate().unwrap();
        with(&|b| b.encounter_step_units = Some(0.5)).validate().unwrap();
        for bad in [
            with(&|b| b.ticks_per_second = 0),
            with(&|b| b.ticks_per_second = 1001),
            with(&|b| b.level = Some(0)),
            with(&|b| b.level = Some(100)),
            with(&|b| b.encounter_step_units = Some(0.0)),
            with(&|b| b.encounter_step_units = Some(-1.0)),
            with(&|b| b.encounter_step_units = Some(f32::NAN)),
            with(&|b| b.encounter_step_units = Some(f32::INFINITY)),
        ] {
            assert!(bad.validate().is_err(), "{:?}", bad.battle);
        }
        // Debug-printing a config shows the fingerprint of the battle data, nothing else of it.
        let shown = format!("{:?}", with(&|_| {}));
        assert!(shown.contains("data_fingerprint") && shown.contains("BattleSettings"), "{shown}");
        assert!(!shown.contains("SyntheticRules"), "{shown}");
    }

    #[test]
    fn the_build_suffix_is_one_validated_token() {
        let ok = ServerConfig::default();
        let with = |s: &str| ServerConfig { build_suffix: Some(s.to_string()), ..ok.clone() };
        let c = with("fmv-ff7/0.1.0+fingerprint-rule/1");
        c.validate().unwrap();
        assert_eq!(c.build_string(), format!("{}+fmv-ff7/0.1.0+fingerprint-rule/1", crate::SERVER_BUILD));
        for bad in ["", "has space", "line\nbreak", "tab\t", "caf\u{e9}", "=", &"x".repeat(MAX_BUILD_SUFFIX + 1)] {
            assert!(with(bad).validate().is_err(), "{bad:?} must be rejected");
        }
        with(&"x".repeat(MAX_BUILD_SUFFIX)).validate().unwrap();
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
            ServerConfig { registrations_per_minute: 0, ..ok.clone() },
            ServerConfig { registrations_per_minute: MAX_REGISTRATIONS_PER_MINUTE + 1, ..ok.clone() },
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
    /// v2 cannot split an `AreaSnapshot`. If this fails after a protocol change, every joiner of
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
