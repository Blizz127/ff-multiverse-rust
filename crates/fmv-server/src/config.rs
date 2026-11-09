//! Server configuration and its validation.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use fmv_ff7_battle::rules::{DEFAULT_BATTLE_SPEED, DEFAULT_TICKS_PER_SECOND};
use fmv_ff7_battle::status::StatusSet;
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

/// Default for `ServerConfig::hello_timeout`. A real client sends `Hello` the moment the connection
/// is up (it has done all its local work before connecting), so a few seconds is generous even over
/// a slow link, and a socket that only sits there frees its slot quickly.
pub const DEFAULT_HELLO_TIMEOUT: Duration = Duration::from_secs(3);

/// Default for `ServerConfig::max_connections_per_ip`: a household or a LAN party behind one NAT,
/// not a farm.
pub const DEFAULT_MAX_CONNECTIONS_PER_IP: usize = 8;

/// Default for `ServerConfig::registrations_per_ip` (per ten minutes).
pub const DEFAULT_REGISTRATIONS_PER_IP: u32 = 10;

/// Default for `ServerConfig::inactivity_timeout`: half an hour without any game input. Long
/// enough for a player who steps away, short enough that a script that only answers pings cannot
/// keep a slot for the day.
pub const DEFAULT_INACTIVITY_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// How a server that can run battles runs them. `None` in [`ServerConfig::battle`] (the FIXTURE
/// server, or an FF7 server without owned battle data) means battles are unsupported and every
/// request is refused with `BattleRejection::Unsupported`.
#[derive(Clone)]
pub struct BattleSettings {
    /// The battle rules over THIS server's own game data (see `fmv_protocol::BattleRules`).
    pub rules: Arc<dyn BattleRules>,
    /// The level every battle member fights at, whatever its own level is; `None` is each member's
    /// own level (its account's character progress). Only the fight is pinned: rewards still go to
    /// each account's own progress.
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
    /// DEBUG, `--debug-limit-full` (refused without `--debug-encounters`, see [`ServerConfig::validate`]):
    /// every member of every battle this server starts begins it with a FULL limit gauge (the game's cap)
    /// instead of its account's own, so that a smoke run can force a Limit command. It changes exactly two
    /// things: the gauge a battle starts with (`BattleMember::limit`: `EncounterStart`, the `battle started`
    /// log line and every client's replay carry it), and the end-of-battle gauge save, which is skipped
    /// (`World::keep_limits`), so no limit gauge is ever stored under it: a gauge that started from a
    /// faked full one is not the account's own, so every account keeps the gauge it really has, even when a
    /// member spent its limit or was knocked out. Off by default.
    pub debug_limit_full: bool,
    /// DEBUG, `--debug-bonus-exp N` (refused without `--debug-encounters`, and by `main` on the default state
    /// directory: see [`ServerConfig::validate`]): every won battle PAYS each member that fought on `N` experience
    /// more than the battle gave it, so that a smoke run can make a new character level up over real processes
    /// (a win pays about 16 of the ~250 a level costs). It changes only what is paid: the payment goes down the
    /// ordinary road (the member's mirror, the saver, the game's own level-up rule, the `battle reward` log line,
    /// which says `debug_bonus_exp=N` and whose `exp=` is what was really paid), while the `BattleEnd` that the
    /// clients verify by replay still carries the rewards the battle itself earned. What is stored afterwards is
    /// indistinguishable from earned experience: only ever run it on a throw-away state directory. 0 is off.
    pub debug_bonus_exp: u32,
    /// DEBUG, `--debug-bonus-limit N` (same rules as `debug_bonus_exp`, and refused together with
    /// `debug_limit_full`, whose early return would make it do nothing): the limit gauge each member is kept with
    /// at the end of a battle is `N` more than the battle left it, up to the game's cap, through the ordinary
    /// `limit kept` save (the log line says `debug_bonus_limit=N`), so that a smoke run can show a non-empty gauge
    /// surviving a server restart over real processes. 0 is off.
    pub debug_bonus_limit: u8,
    /// DEBUG, `--debug-status LIST` (refused without `--debug-encounters`, and when the set is not one a battle can
    /// start with: see [`StatusSet::is_valid_start`]): every member of every battle this server starts, the canon
    /// companions and the players alike, begins it with these statuses, so that a smoke run can show a status applied,
    /// travelling and replayed over real processes (a real battle starts clean, and the first mission's enemies inflict
    /// none). It changes exactly one thing, the statuses a battle starts with (`BattleMember::status`: `EncounterStart`,
    /// the `battle started` log line and every client's replay carry it); nothing is stored. Empty (off) by default.
    pub debug_status: StatusSet,
    /// When the canon companions act (protocol v9 companions, see `crate::companion`, "Pacing"): a companion with a full
    /// gauge waits for every living, connected player that still owes a turn, at most `turn_wait_ms` after that
    /// player's own gauge filled and at most `max_wait_ms` in all. `--companion-turn-wait-ms` / `--companion-max-wait-ms`;
    /// `Pacing::OFF` (a max wait of 0) makes them act the instant their gauge fills.
    pub companion_pacing: crate::companion::Pacing,
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
            debug_limit_full: false,
            debug_bonus_exp: 0,
            debug_bonus_limit: 0,
            debug_status: StatusSet::EMPTY,
            companion_pacing: crate::companion::Pacing::default(),
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
            .field("debug_limit_full", &self.debug_limit_full)
            .field("debug_bonus_exp", &self.debug_bonus_exp)
            .field("debug_bonus_limit", &self.debug_bonus_limit)
            .field("debug_status", &self.debug_status)
            .field("companion_pacing", &self.companion_pacing)
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
    /// Most simultaneous connections ONE address may hold, handshaking ones included
    /// ([`DEFAULT_MAX_CONNECTIONS_PER_IP`]; an IPv6 /64 counts as one address, an IPv4-mapped
    /// IPv6 address as its IPv4 address). Beyond it a connection is refused with
    /// `RejectCode::TooManyConnections` before it takes a slot, so one host cannot fill the server
    /// with silent sockets or idle accounts. `None` lifts the cap. Players behind one NAT share it.
    pub max_connections_per_ip: Option<usize>,
    /// A new connection must send `Hello` within this time ([`DEFAULT_HELLO_TIMEOUT`]). Until it
    /// does, the connection holds a slot (and a place in its address's share) for nothing.
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
    ///
    /// Silence is only half of "idle": a client that keeps pinging is not silent. See
    /// [`ServerConfig::inactivity_timeout`] for that half.
    pub idle_timeout: Option<Duration>,
    /// Drop joined clients that send no GAME INPUT for this long, however often they ping
    /// (`Reject { code: IdleTimeout, "closed for inactivity ..." }`). Input is anything but a `Ping`
    /// (a move, a party request, a battle command); the reference client sends a `Ping` every second
    /// and a move only when it walks. This is the policy for idle-but-pinging sessions: without it
    /// an account that does nothing but answer pings holds its slot forever, and 64 of them (the
    /// default `max_connections`, from at least eight addresses given `max_connections_per_ip`)
    /// would hold the whole server. A player who steps away simply logs in again. Defaults to
    /// [`DEFAULT_INACTIVITY_TIMEOUT`]; `None` disables it.
    pub inactivity_timeout: Option<Duration>,
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
    /// Most accounts the store holds (`fmv_persist::DEFAULT_MAX_ACCOUNTS` by default). A server at the
    /// limit refuses new registrations with `RejectCode::RegistrationClosed` before any disk work;
    /// logins and everything else are unaffected, and lowering it below the number of existing
    /// accounts keeps them all. Registration spam therefore cannot grow the state directory
    /// without bound.
    pub max_accounts: u64,
    /// Accounts the whole server creates per minute at most (a sliding window over all
    /// connections), so a flood of connections cannot fill the store with accounts. Only accounts
    /// that are actually created count: a registration refused for its name, a name that is taken or
    /// a peer that vanished costs nobody a place.
    pub registrations_per_minute: u32,
    /// Accounts one address may create per ten minutes ([`DEFAULT_REGISTRATIONS_PER_IP`]; an IPv6 /64
    /// counts as one address). Without it one host can use up the whole server's
    /// `registrations_per_minute` and lock everybody else out of registering. `None` lifts the limit.
    pub registrations_per_ip: Option<u32>,
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
            max_connections_per_ip: Some(DEFAULT_MAX_CONNECTIONS_PER_IP),
            hello_timeout: DEFAULT_HELLO_TIMEOUT,
            join_timeout: Duration::from_secs(30),
            write_timeout: Duration::from_secs(10),
            idle_timeout: Some(DEFAULT_IDLE_TIMEOUT),
            inactivity_timeout: Some(DEFAULT_INACTIVITY_TIMEOUT),
            keepalive: Some(DEFAULT_KEEPALIVE),
            max_msgs_per_sec: 120,
            outbound_queue: 256,
            shutdown_grace: Duration::from_secs(5),
            state_dir: None,
            allow_registration: true,
            max_accounts: fmv_persist::DEFAULT_MAX_ACCOUNTS,
            registrations_per_minute: DEFAULT_REGISTRATIONS_PER_MINUTE,
            registrations_per_ip: Some(DEFAULT_REGISTRATIONS_PER_IP),
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
        if let Some(n) = self.max_connections_per_ip {
            if !(1..=MAX_CONNECTIONS_LIMIT).contains(&n) {
                return bad(format!(
                    "max_connections_per_ip must be 1..={MAX_CONNECTIONS_LIMIT} (got {n}; use None for no cap)"
                ));
            }
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
        if self.inactivity_timeout.is_some_and(|d| d.is_zero()) {
            return bad("inactivity_timeout must be non-zero (use None to disable)".into());
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
        if self.max_accounts == 0 {
            return bad("max_accounts must be at least 1 (use allow_registration = false to close registration)".into());
        }
        if let Some(n) = self.registrations_per_ip {
            if !(1..=MAX_REGISTRATIONS_PER_MINUTE).contains(&n) {
                return bad(format!(
                    "registrations_per_ip must be 1..={MAX_REGISTRATIONS_PER_MINUTE} (got {n}; use None for no limit)"
                ));
            }
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
            // A debug aid never runs on its own: it belongs to a server that was asked for debug encounters.
            if b.debug_limit_full && !self.debug_encounters {
                return bad("debug_limit_full (--debug-limit-full) is a debug flag and needs debug_encounters (--debug-encounters) too".into());
            }
            // The two bonus flags make up numbers that are then stored like earned ones: debug aids too.
            if (b.debug_bonus_exp > 0 || b.debug_bonus_limit > 0) && !self.debug_encounters {
                return bad(
                    "debug_bonus_exp and debug_bonus_limit (--debug-bonus-exp, --debug-bonus-limit) are debug flags and need debug_encounters (--debug-encounters) too"
                        .into(),
                );
            }
            // A status every member starts with is a debug aid, and must be a set the engine can start with.
            if !b.debug_status.is_empty() && !self.debug_encounters {
                return bad("debug_status (--debug-status) is a debug flag and needs debug_encounters (--debug-encounters) too".into());
            }
            if !b.debug_status.is_valid_start() {
                return bad(format!(
                    "debug_status (--debug-status) {} is not a set a battle can start with (only the statuses the battle plays, and never Haste with Slow or Sadness with Fury)",
                    b.debug_status
                ));
            }
            // `keep_limits` returns early under --debug-limit-full, so a limit bonus would silently do nothing.
            if b.debug_bonus_limit > 0 && b.debug_limit_full {
                return bad(
                    "debug_bonus_limit (--debug-bonus-limit) cannot be combined with debug_limit_full (--debug-limit-full): no limit gauge is stored under the second, so the first would do nothing"
                        .into(),
                );
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
    use fmv_protocol::{encode, NpcSnapshot, ServerMsg, MAX_FRAME, MAX_NPCS_PER_AREA};

    #[test]
    fn default_is_valid_and_matches_spec_defaults() {
        let c = ServerConfig::default();
        c.validate().unwrap();
        assert_eq!(c.bind.port(), DEFAULT_DEV_PORT);
        assert!(c.bind.ip().is_loopback());
        assert_eq!(c.tick_hz, 20);
        assert_eq!(c.max_connections, 64);
        assert_eq!(c.area_key, FIXTURE_AREA_KEY);
        assert_eq!(c.hello_timeout, DEFAULT_HELLO_TIMEOUT);
        assert_eq!(c.hello_timeout, Duration::from_secs(3));
        // FIXTURE keeps the plain version as its build string.
        assert_eq!(c.build_suffix, None);
        assert_eq!(c.build_string(), crate::SERVER_BUILD);
        // Accounts: an ephemeral store, open registration, a three-member party with 60 s invites
        // (the party clock is in milliseconds).
        assert_eq!(c.state_dir, None);
        assert!(c.allow_registration);
        assert_eq!(c.registrations_per_minute, DEFAULT_REGISTRATIONS_PER_MINUTE);
        assert_eq!(c.registrations_per_ip, Some(DEFAULT_REGISTRATIONS_PER_IP));
        assert_eq!(c.max_connections_per_ip, Some(DEFAULT_MAX_CONNECTIONS_PER_IP));
        assert_eq!(c.max_accounts, fmv_persist::DEFAULT_MAX_ACCOUNTS);
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
    fn the_debug_limit_flag_is_off_by_default_and_never_runs_without_debug_encounters() {
        use fmv_protocol::SyntheticRules;
        assert!(!BattleSettings::new(Arc::new(SyntheticRules::default())).debug_limit_full, "off unless asked for");
        let with = |debug_encounters: bool, limit_full: bool| {
            let mut b = BattleSettings::new(Arc::new(SyntheticRules::default()));
            b.debug_limit_full = limit_full;
            ServerConfig { battle: Some(b), debug_encounters, ..ServerConfig::default() }
        };
        with(true, true).validate().unwrap();
        with(true, false).validate().unwrap();
        with(false, false).validate().unwrap();
        let err = with(false, true).validate().expect_err("a debug flag without --debug-encounters is refused").to_string();
        assert!(err.contains("--debug-limit-full") && err.contains("--debug-encounters"), "{err}");
        assert!(format!("{:?}", with(true, true).battle).contains("debug_limit_full: true"));
    }

    #[test]
    fn the_debug_status_flag_is_off_by_default_needs_debug_encounters_and_only_takes_a_set_a_battle_can_start_with() {
        use fmv_ff7_battle::status::Status;
        use fmv_protocol::SyntheticRules;
        assert!(BattleSettings::new(Arc::new(SyntheticRules::default())).debug_status.is_empty(), "off unless asked for");
        let with = |debug_encounters: bool, statuses: &[Status]| {
            let mut b = BattleSettings::new(Arc::new(SyntheticRules::default()));
            b.debug_status = statuses.iter().copied().collect();
            ServerConfig { battle: Some(b), debug_encounters, ..ServerConfig::default() }
        };
        with(true, &[Status::Poison, Status::Silence]).validate().unwrap();
        with(true, &[]).validate().unwrap();
        with(false, &[]).validate().unwrap();
        let err = with(false, &[Status::Poison]).validate().expect_err("a debug flag without --debug-encounters is refused").to_string();
        assert!(err.contains("--debug-status") && err.contains("--debug-encounters"), "{err}");
        // Haste with Slow, Sadness with Fury and a status the battle does not play are not sets a member starts with.
        for bad in [&[Status::Haste, Status::Slow][..], &[Status::Sadness, Status::Fury], &[Status::Confusion], &[Status::Death]] {
            let err = with(true, bad).validate().expect_err("not a set a battle can start with").to_string();
            assert!(err.contains("--debug-status") && err.contains("cannot start") || err.contains("not a set a battle can start with"), "{bad:?}: {err}");
        }
        assert!(format!("{:?}", with(true, &[Status::Poison]).battle).contains("debug_status: StatusSet(8)"), "{:?}", with(true, &[Status::Poison]).battle);
    }

    #[test]
    fn the_debug_bonus_flags_are_off_by_default_need_debug_encounters_and_never_meet_the_full_gauge_flag() {
        use fmv_protocol::SyntheticRules;
        let off = BattleSettings::new(Arc::new(SyntheticRules::default()));
        assert_eq!((off.debug_bonus_exp, off.debug_bonus_limit), (0, 0), "off unless asked for");
        let with = |debug_encounters: bool, exp: u32, limit: u8, full: bool| {
            let mut b = BattleSettings::new(Arc::new(SyntheticRules::default()));
            (b.debug_bonus_exp, b.debug_bonus_limit, b.debug_limit_full) = (exp, limit, full);
            ServerConfig { battle: Some(b), debug_encounters, ..ServerConfig::default() }
        };
        with(true, 300, 0, false).validate().unwrap();
        with(true, 0, 100, false).validate().unwrap();
        with(true, 300, 100, false).validate().unwrap();
        with(false, 0, 0, false).validate().unwrap();
        for (exp, limit) in [(300, 0), (0, 100), (300, 100)] {
            let err = with(false, exp, limit, false).validate().expect_err("a debug flag without --debug-encounters is refused").to_string();
            assert!(err.contains("--debug-bonus") && err.contains("--debug-encounters"), "{err}");
        }
        // The full-gauge flag skips every limit save, so a limit bonus next to it would do nothing: refused.
        let err = with(true, 0, 100, true).validate().expect_err("the two limit flags do not meet").to_string();
        assert!(err.contains("--debug-bonus-limit") && err.contains("--debug-limit-full"), "{err}");
        with(true, 300, 0, true).validate().unwrap();
        let shown = format!("{:?}", with(true, 300, 100, false).battle);
        assert!(shown.contains("debug_bonus_exp: 300") && shown.contains("debug_bonus_limit: 100"), "{shown}");
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
            ServerConfig { max_accounts: 0, ..ok.clone() },
            ServerConfig { max_connections_per_ip: Some(0), ..ok.clone() },
            ServerConfig { max_connections_per_ip: Some(MAX_CONNECTIONS_LIMIT + 1), ..ok.clone() },
            ServerConfig { registrations_per_ip: Some(0), ..ok.clone() },
            ServerConfig { registrations_per_ip: Some(MAX_REGISTRATIONS_PER_MINUTE + 1), ..ok.clone() },
            ServerConfig { inactivity_timeout: Some(Duration::ZERO), ..ok.clone() },
        ] {
            assert!(c.validate().is_err(), "{c:?} should be rejected");
        }
        ServerConfig { tick_hz: MAX_TICK_HZ, max_connections: MAX_CONNECTIONS_LIMIT, ..ok.clone() }
            .validate()
            .unwrap();
        // Every protection can be switched off explicitly.
        ServerConfig {
            idle_timeout: None,
            keepalive: None,
            inactivity_timeout: None,
            max_connections_per_ip: None,
            registrations_per_ip: None,
            ..ok
        }
        .validate()
        .unwrap();
    }

    /// The idle policy has two halves and both are on by default: silence is cut after
    /// `idle_timeout` (a peer that vanished), and a session that only answers pings is cut after
    /// `inactivity_timeout` (a squatter), which must be longer than the silence window and finite.
    #[test]
    fn pinging_without_playing_does_not_hold_a_slot_forever() {
        let c = ServerConfig::default();
        let inactivity = c.inactivity_timeout.expect("on by default");
        assert_eq!(inactivity, DEFAULT_INACTIVITY_TIMEOUT);
        assert!(inactivity > c.idle_timeout.expect("on by default"));
        assert!(inactivity <= Duration::from_secs(60 * 60), "a squatter must not keep a slot for hours");
        assert!(inactivity >= Duration::from_secs(5 * 60), "a player who steps away must not be kicked at once");
        // A handshake that goes nowhere is cut in seconds, and one address holds a small share.
        assert!(c.hello_timeout <= Duration::from_secs(5));
        let per_ip = c.max_connections_per_ip.expect("on by default");
        assert!(per_ip >= 2 && per_ip * 4 <= c.max_connections, "one address must not be able to fill the server");
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
        // The area's NPCs (static, at most `MAX_NPCS_PER_AREA`) ride in the same frame: the worst
        // case counts them too (the server clips a name to 24 characters, a model and an animation
        // to 16).
        let npc = |i: u32| NpcSnapshot {
            id: u32::MAX - i,
            name: wide(24),
            model: wide(16),
            idle_animation: Some(wide(16)),
            pos: Pos::new(-f32::MAX, f32::MIN_POSITIVE, -f32::MAX),
            facing: -f32::MAX,
        };
        let snapshot = |n: usize| ServerMsg::AreaSnapshot {
            area: AreaId { game: GameId::Fixture, key: "x".repeat(64) },
            tick: u64::MAX,
            entities: (0..n as u64).map(entity).collect(),
            npcs: (0..MAX_NPCS_PER_AREA as u32).map(npc).collect(),
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
