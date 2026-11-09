//! `fmv-server`: authoritative FF Multiverse server (its protocol is `fmv_protocol::PROTOCOL_VERSION`).
//!
//! Thin wrapper over the `fmv_server` library: parse flags, pick the game adapter, install
//! signal handlers, run. Logs go to stderr; stdout carries exactly one line, the READY line.
//!
//! `--game ff7` hosts a real FF7 field (default `md1stin`) from the owner's own content: the
//! imported library (`--ff7-library`, `--ff7-set`) or a `flevel.lgp` read directly
//! (`--ff7-flevel`). Both are opened read-only, and no asset byte ever leaves this process.
//!
//! Battles (protocol v3, with character progress since v5): an FF7 server opened from a library also
//! loads that set's `scene.bin` and `kernel.bin` and runs shared, server-authoritative battles
//! (random encounters in a field that has an encounter table, and on request with
//! `--debug-encounters`). Every account fights as its own character at its own level and a victory
//! pays it (level, experience, AP and gil are kept per account in the state directory). The FIXTURE
//! server and a server over a direct `--ff7-flevel` have no battle data and refuse every battle
//! request.
//!
//! Players have accounts: the server keeps them (with avatar profiles and progression) in a state
//! directory it holds locked while it runs (`--state-dir`, default `<state>/server`;
//! `--ephemeral` keeps nothing).

use std::io::IsTerminal;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, ValueEnum};
use fmv_adapter_api::GameAdapter;
use fmv_ff7_battle::rules::{DEFAULT_BATTLE_SPEED, DEFAULT_TICKS_PER_SECOND};
use fmv_fixture::{FixtureAdapter, FIXTURE_AREA_KEY};
use fmv_protocol::DEFAULT_DEV_PORT;
use fmv_server::ff7::{self, Ff7Labels, Ff7Source, SharedFf7, DEFAULT_FF7_AREA};
use fmv_server::{
    BattleSettings, SceneSettings, ServerConfig, DEFAULT_HELLO_TIMEOUT, DEFAULT_IDLE_TIMEOUT, DEFAULT_INACTIVITY_TIMEOUT, DEFAULT_MAX_CONNECTIONS_PER_IP,
    DEFAULT_REGISTRATIONS_PER_IP, DEFAULT_REGISTRATIONS_PER_MINUTE, MAX_CONNECTIONS_LIMIT, MAX_REGISTRATIONS_PER_MINUTE, MAX_TICK_HZ,
};
use tracing::{info, warn};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum GameKind {
    /// Synthetic FIXTURE area. NOT FF7 support.
    Fixture,
    /// Synthetic FIXTURE rooms: several connected areas (`hall`, `yard`) with exits, to try walking
    /// between areas without owned data. NOT FF7 support.
    FixtureRooms,
    /// Real FF7 content from the owner's own copy (see --ff7-library / --ff7-flevel).
    Ff7,
}

const AFTER_HELP: &str = "\
FF7 CONTENT:
  --game ff7 hosts the area `md1stin` (override with --area) from the owner's own data, read-only.
  Default source: the library fmv-import wrote under $FMV_STATE_DIR (else the user data directory);
  --ff7-library names another state directory, --ff7-set picks a set when it holds several, and
  --ff7-flevel reads one flevel.lgp directly. Passing any --ff7-* source flag selects --game ff7.
  The area fingerprint binds edition, region and revision: clients must hold the same copy.

ACCOUNTS:
  Clients log in with a saved token or register an account (display name). Accounts, avatar
  profiles and progression live in the state directory, which this server holds locked while it
  runs (a second server on the same directory is refused). Default: `server/` inside $FMV_STATE_DIR
  (else the user data directory's ff-multiverse); --state-dir DIR uses DIR itself. --ephemeral keeps
  accounts only until the server stops. --no-registration refuses new accounts.

LIMITS (the defaults suit a small private server; the protocol is not encrypted, so keep it on a network you trust):
  A connection that never sends Hello is cut after --hello-timeout-secs. One address (an IPv6 /64
  counts as one; players behind one NAT share it) may hold --max-connections-per-ip connections and
  create --registrations-per-ip accounts per 10 minutes; the whole server creates at most
  --registrations-per-minute accounts per minute and holds at most --max-accounts, after which
  registration is refused and logins go on. Only accounts that are actually created use up a
  registration limit. A joined client that sends nothing at all is dropped after --idle-timeout-secs
  (a vanished peer); one that only answers pings and sends no game input (no move, party request or
  battle command) is dropped after --inactivity-timeout-secs (a squatter: it would hold its slot
  forever), and logs in again when its player is back. 0 turns a limit off.

BATTLES:
  With --game ff7 from a library, the server also reads that set's scene.bin and kernel.bin
  (read-only) and runs shared battles on the deterministic engine: the server steps it, every
  player in the battle is frozen in the field and replays it from their OWN game files (only ids,
  seeds and levels are sent). A battle starts for the whole party from a random encounter in a
  field that has an encounter table (md1stin has one), or when a player asks for one and the server
  runs with --debug-encounters (off by default). Without battle data (--game fixture, a direct
  --ff7-flevel) every battle request is refused.

CHARACTER PROGRESS:
  Every account has its own progress in the game, kept in the state directory beside (and apart
  from) its story state: the character it fights as (the combat slot of its player character, chosen
  once when the character is created: a canon party member other than Cloud; an account that predates
  player characters keeps the character its look named, Cloud without one) and that character's
  level, experience, AP and gil. A battle's members fight as their own character at their
  own level; a victory pays each member that fought on and the server applies it with the game's own
  level-up rule (the experience curve of the character's kernel data), tells the player
  (`CharacterProgress`, which it also gets when it joins) and saves it. An account that has earned
  nothing yet starts at its character's starting level and experience from the game data.
  --battle-speed, --battle-tick-rate, --battle-level, --battle-seed and --debug-formation pin the
  pace, level, seed and fight for reproducible runs; --no-random-encounters turns the rolls off.

DEBUG FLAGS (smoke-test aids; leave them off on a server real players use):
  --debug-encounters lets a player start a battle on request. --debug-limit-full (refused unless
  --debug-encounters is given too, and warned about at start-up) makes every member of every battle
  start with a FULL limit gauge instead of its account's own, so that a smoke run can force a Limit
  command. It changes exactly two things: the gauge a battle starts with (the `battle started ...
  limits=[..]` log line shows it), and the end-of-battle gauge save, which is skipped (`limit not
  kept ... reason=debug-limit-full`): a limit gauge is never stored under it, so every account keeps
  the gauge it really has, even after a member spent its limit or was knocked out.
  --debug-status LIST (refused unless --debug-encounters is given too, and warned about at start-up) makes
  every member of every battle start with those statuses (comma-separated names of the ones a battle plays:
  Sleep, Poison, Silence, Haste, Slow, Stop, Regen, Barrier, MBarrier, Sadness, Fury, Paralysis, Darkness;
  never Haste with Slow or Sadness with Fury), so that a smoke run can show a status applied, announced to
  every client and replayed by it with no divergence: a real battle starts clean and the first mission's
  enemies inflict none. It changes only what a battle starts with (the `battle started ... statuses=[..]` log
  line shows it; the `status inflicted` / `status ended` lines follow them); nothing is stored.
  --debug-bonus-exp EXP and --debug-bonus-limit GAUGE (each refused unless --debug-encounters is given
  too, refused on the DEFAULT state directory (name a throw-away one with --state-dir, or use
  --ephemeral), warned about at start-up and on every use; --debug-bonus-limit is also refused with
  --debug-limit-full) are the opposite of --debug-limit-full: they make up numbers and STORE them, so
  that a smoke run over real processes can level a new character up (a win pays about 16 of the ~250
  experience a level costs) and fill a limit gauge, then prove both survive a real server restart.
  Every won battle pays each member that fought on EXP experience more than the battle gave it (the
  `battle reward ... exp=N debug_bonus_exp=EXP` line says what was really paid; the clients' replay of
  the battle still verifies the rewards the battle itself earned), and every member's end-of-battle
  limit gauge is kept GAUGE higher than the battle left it, up to the cap (`limit kept ...
  debug_bonus_limit=GAUGE`). What is stored is indistinguishable from earned numbers.";

#[derive(Parser, Debug)]
#[command(name = "fmv-server", version, about = "Authoritative FF Multiverse server", after_help = AFTER_HELP)]
struct Cli {
    /// Address to listen on. Port 0 picks a free port (reported on the READY line).
    #[arg(long, value_name = "ADDR", default_value_t = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), DEFAULT_DEV_PORT))]
    bind: SocketAddr,

    /// Which game adapter hosts the area.
    #[arg(long, value_enum, default_value_t = GameKind::Fixture)]
    game: GameKind,

    /// Key of the entry area (default: `plaza` for the FIXTURE game, `hall` for the FIXTURE rooms,
    /// `md1stin` for FF7). Every player joins it; the areas its exits lead to are loaded on demand.
    #[arg(long, value_name = "KEY")]
    area: Option<String>,

    /// Broadcast ticks per second.
    #[arg(long, value_name = "HZ", default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=i64::from(MAX_TICK_HZ)))]
    tick_hz: u32,

    /// Owned FF7 library: the state directory fmv-import wrote (default: $FMV_STATE_DIR, else the
    /// user data directory). Selects `--game ff7`.
    #[arg(long, value_name = "STATE_DIR", conflicts_with = "ff7_flevel")]
    ff7_library: Option<PathBuf>,

    /// Library set to host from (default: the only FF7 PC set). Selects `--game ff7`.
    #[arg(long, value_name = "SET_ID", conflicts_with = "ff7_flevel")]
    ff7_set: Option<String>,

    /// Read this flevel.lgp directly (read-only) instead of the library. Selects `--game ff7`.
    #[arg(long, value_name = "PATH")]
    ff7_flevel: Option<PathBuf>,

    /// Edition label for --ff7-flevel (default `unknown`). It is part of the area fingerprint:
    /// match the library set of the peers (`fmv-import list` shows it).
    #[arg(long, value_name = "TEXT", requires = "ff7_flevel", conflicts_with_all = ["ff7_library", "ff7_set"])]
    ff7_edition: Option<String>,

    /// Region label for --ff7-flevel (default `unknown`; part of the area fingerprint).
    #[arg(long, value_name = "TEXT", requires = "ff7_flevel", conflicts_with_all = ["ff7_library", "ff7_set"])]
    ff7_region: Option<String>,

    /// Language label for --ff7-flevel (default `unknown`).
    #[arg(long, value_name = "TEXT", requires = "ff7_flevel", conflicts_with_all = ["ff7_library", "ff7_set"])]
    ff7_language: Option<String>,

    /// Revision label for --ff7-flevel (default `unknown`; part of the area fingerprint).
    #[arg(long, value_name = "TEXT", requires = "ff7_flevel", conflicts_with_all = ["ff7_library", "ff7_set"])]
    ff7_revision: Option<String>,

    /// Directory of the account store (accounts, avatar profiles, progression). Default: `server/`
    /// inside $FMV_STATE_DIR, else inside the user data directory's `ff-multiverse`.
    #[arg(long, value_name = "DIR", conflicts_with = "ephemeral")]
    state_dir: Option<PathBuf>,

    /// Keep accounts only until this server stops (a temporary store that is deleted at exit).
    #[arg(long)]
    ephemeral: bool,

    /// Refuse new registrations: only tokens that were already issued can log in.
    #[arg(long)]
    no_registration: bool,

    /// Maximum concurrent connections.
    #[arg(long, value_name = "N", default_value_t = 64, value_parser = clap::value_parser!(u32).range(1..=MAX_CONNECTIONS_LIMIT as i64))]
    max_connections: u32,

    /// Most simultaneous connections from ONE address (an IPv6 /64 counts as one address; players
    /// behind one NAT share it). A connection beyond it is refused before it takes a slot. 0 = no cap.
    #[arg(long, value_name = "N", default_value_t = DEFAULT_MAX_CONNECTIONS_PER_IP as u32,
          value_parser = clap::value_parser!(u32).range(0..=MAX_CONNECTIONS_LIMIT as i64))]
    max_connections_per_ip: u32,

    /// Accounts ONE address may create per 10 minutes (an IPv6 /64 counts as one address). 0 = no limit.
    #[arg(long, value_name = "N", default_value_t = DEFAULT_REGISTRATIONS_PER_IP,
          value_parser = clap::value_parser!(u32).range(0..=i64::from(MAX_REGISTRATIONS_PER_MINUTE)))]
    registrations_per_ip: u32,

    /// Accounts the whole server creates per minute at most.
    #[arg(long, value_name = "N", default_value_t = DEFAULT_REGISTRATIONS_PER_MINUTE,
          value_parser = clap::value_parser!(u32).range(1..=i64::from(MAX_REGISTRATIONS_PER_MINUTE)))]
    registrations_per_minute: u32,

    /// Most accounts the store holds; at the limit new registrations are refused and logins go on.
    #[arg(long, value_name = "N", default_value_t = fmv_persist::DEFAULT_MAX_ACCOUNTS,
          value_parser = clap::value_parser!(u64).range(1..))]
    max_accounts: u64,

    /// A new connection must send Hello within this many seconds.
    #[arg(long, value_name = "SECS", default_value_t = DEFAULT_HELLO_TIMEOUT.as_secs(),
          value_parser = clap::value_parser!(u64).range(1..))]
    hello_timeout_secs: u64,

    /// Disconnect joined clients that send no message for this many seconds (0 = never). Clients
    /// must send something, for example a Ping (the reference client pings every second), well
    /// inside this window. The default frees the slot and avatar of silent or vanished peers.
    #[arg(long, value_name = "SECS", default_value_t = DEFAULT_IDLE_TIMEOUT.as_secs())]
    idle_timeout_secs: u64,

    /// Disconnect joined clients that send no game input (only Pings) for this many seconds
    /// (0 = never). Without it an account that merely answers pings holds its slot forever.
    #[arg(long, value_name = "SECS", default_value_t = DEFAULT_INACTIVITY_TIMEOUT.as_secs())]
    inactivity_timeout_secs: u64,

    /// Let players start a battle on request (the client's B key, `--battle-auto` bots). OFF by
    /// default: a normal server only starts battles from random encounters. Needs battle data
    /// (an FF7 library with scene.bin and kernel.bin).
    #[arg(long)]
    debug_encounters: bool,

    /// Let players put themselves in a story scene's field at the scene's own story moment, on a made-up story that is never
    /// saved (the client's `--scene-preview` bot flag and its F6 key). OFF by default: a normal server offers a scene only to a
    /// player whose real story is at its moment. FF7 only.
    #[arg(long)]
    debug_scene_preview: bool,

    /// Do not roll a field's random-encounter table as players walk.
    #[arg(long)]
    no_random_encounters: bool,

    /// FF7 "Battle Speed" of every battle: 0 is fastest, 255 slowest.
    #[arg(long, value_name = "N", default_value_t = DEFAULT_BATTLE_SPEED)]
    battle_speed: u8,

    /// Engine ticks per second of battle time (1-1000). More makes battles pass faster.
    #[arg(long, value_name = "TPS", default_value_t = DEFAULT_TICKS_PER_SECOND, value_parser = clap::value_parser!(u32).range(1..=1000))]
    battle_tick_rate: u32,

    /// The level every battle member fights at, whatever its own level is (default: each member
    /// fights at its account's own level). Only the fight is pinned: rewards still go to each
    /// account's own progress.
    #[arg(long, value_name = "LEVEL", value_parser = clap::value_parser!(u8).range(1..=99))]
    battle_level: Option<u8>,

    /// Use this seed for every battle instead of a fresh one (reproducible runs).
    #[arg(long, value_name = "SEED")]
    battle_seed: Option<u64>,

    /// The enemy formation (`scene * 4 + slot`) a debug encounter uses when the request names none.
    /// Default: a weighted pick from the field's encounter table, else the first formation.
    #[arg(long, value_name = "ID")]
    debug_formation: Option<u16>,

    /// DEBUG, needs --debug-encounters: every member of every battle starts with a FULL limit gauge
    /// instead of its account's own, so that a smoke run can force a Limit command, and no limit gauge
    /// is ever stored under it (the end-of-battle gauge save is skipped), so a debug run can never
    /// overwrite a real account's gauge. Logged as a warning at start-up.
    #[arg(long, requires = "debug_encounters")]
    debug_limit_full: bool,

    /// DEBUG, needs --debug-encounters: every member of every battle (the canon companions and the players) starts it
    /// with these statuses, a comma-separated list of the names of the ones a battle plays (Sleep, Poison, Silence,
    /// Haste, Slow, Stop, Regen, Barrier, MBarrier, Sadness, Fury, Paralysis, Darkness; never Haste with Slow or Sadness
    /// with Fury), so that a smoke run can show a status applied, announced and replayed over real processes. Nothing is
    /// stored. Logged as a warning at start-up.
    #[arg(long, value_name = "LIST", requires = "debug_encounters", value_parser = parse_status_list)]
    debug_status: Option<fmv_ff7_battle::status::StatusSet>,

    /// DEBUG, needs --debug-encounters and an explicit --state-dir (or --ephemeral): every won battle pays each
    /// member that fought on EXP experience MORE than it earned, through the ordinary save path, so that a smoke run
    /// can make a new character level up over real processes. What is stored is indistinguishable from earned
    /// experience: only ever run it on a throw-away state directory (refused on the default one). Logged as a
    /// warning at start-up and on every payment.
    #[arg(long, value_name = "EXP", requires = "debug_encounters", value_parser = clap::value_parser!(u32).range(1..))]
    debug_bonus_exp: Option<u32>,

    /// DEBUG, same rules as --debug-bonus-exp, and refused together with --debug-limit-full: the limit gauge
    /// every member is kept with at the end of a battle is GAUGE (1-255) more than the battle left it, up to the
    /// cap, through the ordinary save path, so that a smoke run can show a non-empty gauge surviving a restart.
    #[arg(long, value_name = "GAUGE", requires = "debug_encounters", conflicts_with = "debug_limit_full", value_parser = clap::value_parser!(u8).range(1..=255))]
    debug_bonus_limit: Option<u8>,

    /// How long the canon companions (Cloud and Barret) wait for a player whose turn has come and who is still choosing,
    /// in milliseconds of battle time, before they act anyway. The players lead every round; the companions follow.
    #[arg(long, value_name = "MS", default_value_t = fmv_server::companion::DEFAULT_TURN_WAIT_MS)]
    companion_turn_wait_ms: u32,

    /// The longest a canon companion holds its full gauge for the players, in milliseconds of battle time. 0 makes the
    /// companions act the instant their gauge fills.
    #[arg(long, value_name = "MS", default_value_t = fmv_server::companion::DEFAULT_MAX_WAIT_MS)]
    companion_max_wait_ms: u32,
}

/// `--debug-status`: a comma-separated list of status names (any case, with or without spaces and hyphens) that a
/// battle can start with. Says which name or pair it cannot take.
fn parse_status_list(list: &str) -> Result<fmv_ff7_battle::status::StatusSet, String> {
    use fmv_ff7_battle::status::{Status, StatusSet, MODELLED};
    let mut set = StatusSet::EMPTY;
    for word in list.split(',').map(str::trim).filter(|w| !w.is_empty()) {
        let status = Status::from_name(word).filter(|s| s.is_modelled()).ok_or_else(|| {
            format!("`{word}` is not a status a battle plays (it plays {MODELLED})")
        })?;
        set.insert(status);
    }
    if set.is_empty() {
        return Err("the list names no status".into());
    }
    if !set.is_valid_start() {
        return Err(format!("{set} cannot all start a battle together (never Haste with Slow, nor Sadness with Fury)"));
    }
    Ok(set)
}

/// `--idle-timeout-secs` and `--inactivity-timeout-secs`: 0 disables the timeout.
fn idle_timeout(secs: u64) -> Option<Duration> {
    (secs > 0).then(|| Duration::from_secs(secs))
}

/// `--max-connections-per-ip` and `--registrations-per-ip`: 0 lifts the limit.
fn limit(n: u32) -> Option<u32> {
    (n > 0).then_some(n)
}

/// Where the account store lives: `None` for `--ephemeral`, an explicit `--state-dir` as given,
/// else `server/` inside the base state directory (`$FMV_STATE_DIR`, else `<user data dir>/ff-multiverse`,
/// the same base the owned-content library uses). `base` resolves that directory (the real one
/// reads the environment; tests pass a fixed answer).
fn server_state_dir(
    explicit: Option<&Path>,
    ephemeral: bool,
    base: impl FnOnce() -> Result<PathBuf, fmv_library::LibraryError>,
) -> Result<Option<PathBuf>, String> {
    if ephemeral {
        return Ok(None);
    }
    if let Some(dir) = explicit.filter(|d| !d.as_os_str().is_empty()) {
        return Ok(Some(dir.to_path_buf()));
    }
    base()
        .map(|base| Some(base.join("server")))
        .map_err(|e| format!("no state directory for the account store: {e} (or use --state-dir / --ephemeral)"))
}

impl Cli {
    /// The game to host. Any `--ff7-*` source flag asks for the FF7 adapter, even next to
    /// `--game fixture`: a synthetic area is never hosted when real content was asked for.
    fn effective_game(&self) -> GameKind {
        let wants_ff7 = self.ff7_library.is_some() || self.ff7_set.is_some() || self.ff7_flevel.is_some();
        if wants_ff7 {
            GameKind::Ff7
        } else {
            self.game
        }
    }

    /// Where the FF7 field archive comes from.
    fn ff7_source(&self) -> Ff7Source {
        match &self.ff7_flevel {
            Some(path) => {
                let unknown = Ff7Labels::default();
                let label = |given: &Option<String>, default: String| given.clone().unwrap_or(default);
                Ff7Source::Flevel {
                    path: path.clone(),
                    labels: Ff7Labels {
                        edition: label(&self.ff7_edition, unknown.edition),
                        region: label(&self.ff7_region, unknown.region),
                        language: label(&self.ff7_language, unknown.language),
                        revision: label(&self.ff7_revision, unknown.revision),
                    },
                }
            }
            None => Ff7Source::Library { state_dir: self.ff7_library.clone(), set: self.ff7_set.clone() },
        }
    }
}

/// Why an adapter could not be set up. Any error type converts into it, so wiring a real adapter
/// can use `?`.
type SetupError = Box<dyn std::error::Error + Send + Sync>;

/// The adapter and what hosting it implies.
struct Hosted {
    adapter: Box<dyn GameAdapter>,
    /// Area to host: `--area`, else the game's default.
    area: String,
    /// Adapter identity for the build string (`None` for FIXTURE).
    build_suffix: Option<String>,
    /// Battle support (`None`: this server cannot run battles: FIXTURE, or no owned battle data).
    battle: Option<BattleSettings>,
    /// Story scenes (`None`: this game has none, FIXTURE).
    scenes: Option<SceneSettings>,
}

impl Cli {
    /// `config` with every limit and timeout the flags set (see the LIMITS section of `--help`).
    fn with_limits(&self, config: ServerConfig) -> ServerConfig {
        ServerConfig {
            max_connections_per_ip: limit(self.max_connections_per_ip).map(|n| n as usize),
            registrations_per_ip: limit(self.registrations_per_ip),
            registrations_per_minute: self.registrations_per_minute,
            max_accounts: self.max_accounts,
            hello_timeout: Duration::from_secs(self.hello_timeout_secs),
            idle_timeout: idle_timeout(self.idle_timeout_secs),
            inactivity_timeout: idle_timeout(self.inactivity_timeout_secs),
            ..config
        }
    }

    /// How battles run, over `rules` (the server's own battle data).
    fn battle_settings(&self, rules: std::sync::Arc<dyn fmv_protocol::BattleRules>) -> BattleSettings {
        BattleSettings {
            level: self.battle_level,
            battle_speed: self.battle_speed,
            ticks_per_second: self.battle_tick_rate,
            fixed_seed: self.battle_seed,
            debug_formation: self.debug_formation,
            random_encounters: !self.no_random_encounters,
            debug_limit_full: self.debug_limit_full,
            debug_status: self.debug_status.unwrap_or_default(),
            debug_bonus_exp: self.debug_bonus_exp.unwrap_or(0),
            debug_bonus_limit: self.debug_bonus_limit.unwrap_or(0),
            companion_pacing: fmv_server::companion::Pacing { turn_wait_ms: self.companion_turn_wait_ms, max_wait_ms: self.companion_max_wait_ms },
            ..BattleSettings::new(rules)
        }
    }

    /// The two bonus flags make up numbers that are then STORED like earned ones, so they refuse to run on the default
    /// state directory (the one a real server keeps its accounts in, next to the owner's library): the operator must name
    /// a throw-away directory (`--state-dir`) or keep nothing (`--ephemeral`).
    fn check_bonus_flags_state(&self) -> Result<(), String> {
        let uses_bonus = self.debug_bonus_exp.is_some() || self.debug_bonus_limit.is_some();
        let explicit = self.state_dir.as_deref().is_some_and(|dir| !dir.as_os_str().is_empty());
        if uses_bonus && !self.ephemeral && !explicit {
            return Err("--debug-bonus-exp and --debug-bonus-limit make up numbers that are then saved like earned ones, so they refuse to run on the \
                        default state directory (the one a real server keeps its accounts in): pass --state-dir <a throw-away directory> or --ephemeral"
                .to_string());
        }
        Ok(())
    }
}

/// The single place where an adapter is chosen. For FF7 it also builds the area index on a
/// blocking thread (the archive is read and hashed once, tens to hundreds of MB), so nothing is
/// bound and no handshake can pay for it.
async fn build_adapter(cli: &Cli) -> Result<Hosted, SetupError> {
    match cli.effective_game() {
        GameKind::Fixture => Ok(Hosted {
            adapter: Box::new(FixtureAdapter::new()),
            area: cli.area.clone().unwrap_or_else(|| FIXTURE_AREA_KEY.to_string()),
            build_suffix: None,
            battle: None,
            scenes: None,
        }),
        GameKind::FixtureRooms => Ok(Hosted {
            adapter: Box::new(fmv_fixture::rooms::RoomsAdapter::new()),
            area: cli.area.clone().unwrap_or_else(|| fmv_fixture::rooms::HALL.to_string()),
            build_suffix: None,
            battle: None,
            scenes: None,
        }),
        GameKind::Ff7 => {
            let opened = ff7::open_ff7(&cli.ff7_source())?;
            info!(source = %opened.origin, "opened the owned FF7 field archive read-only");
            let report = ff7::prewarm(&opened.adapter).await?;
            info!(areas = report.areas, skipped = report.skipped, elapsed = ?report.elapsed, "indexed the FF7 field archive");
            // The battle data comes from the same library set. A server that was asked for debug
            // encounters must have it; any other server just runs without battles.
            let battle = match ff7::open_battle_data(&cli.ff7_source()) {
                Ok((data, origin)) => {
                    info!(
                        source = %origin,
                        fingerprint = ?fmv_protocol::BattleRules::data_fingerprint(&*data),
                        "opened the owned FF7 battle data (scene.bin, kernel.bin) read-only"
                    );
                    Some(cli.battle_settings(data))
                }
                Err(e) if cli.debug_encounters => return Err(format!("--debug-encounters needs owned battle data: {e}").into()),
                Err(e) => {
                    warn!("no battle data, so battles are unsupported on this server: {e}");
                    None
                }
            };
            // FF7's story scenes read the same field archive; the debug preview is off unless asked for.
            let scenes = SceneSettings {
                host: std::sync::Arc::new(fmv_server::scene_host::Ff7SceneHost::new(std::sync::Arc::clone(&opened.adapter))),
                debug_preview: cli.debug_scene_preview,
            };
            Ok(Hosted {
                adapter: Box::new(SharedFf7(opened.adapter)),
                area: cli.area.clone().unwrap_or_else(|| DEFAULT_FF7_AREA.to_string()),
                build_suffix: Some(ff7::build_tag()),
                battle,
                scenes: Some(scenes),
            })
        }
    }
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .init();
}

/// Install the SIGINT/SIGTERM handlers immediately and return the future that resolves when
/// either arrives. `tokio::signal::ctrl_c()` only registers on first poll, so the handlers are
/// created here, before the READY line can be printed.
#[cfg(unix)]
fn shutdown_signal() -> std::io::Result<impl std::future::Future<Output = ()>> {
    use tokio::signal::unix::{signal, SignalKind};
    let mut sigint = signal(SignalKind::interrupt())?;
    let mut sigterm = signal(SignalKind::terminate())?;
    Ok(async move {
        tokio::select! {
            _ = sigint.recv() => info_signal("SIGINT"),
            _ = sigterm.recv() => info_signal("SIGTERM"),
        }
    })
}

#[cfg(not(unix))]
fn shutdown_signal() -> std::io::Result<impl std::future::Future<Output = ()>> {
    Ok(async {
        let _ = tokio::signal::ctrl_c().await;
        info_signal("Ctrl-C");
    })
}

fn info_signal(name: &str) {
    tracing::info!(signal = name, "received signal");
}

async fn real_main(cli: Cli) -> Result<(), String> {
    // Before anything is built or bound: a debug flag that fabricates stored numbers never touches the default state.
    cli.check_bonus_flags_state()?;
    let hosted = build_adapter(&cli).await.map_err(|e| e.to_string())?;
    let state_dir = server_state_dir(cli.state_dir.as_deref(), cli.ephemeral, || fmv_library::resolve_state_dir(None))?;
    // The library says the same about --debug-encounters; the flag lives in the battle settings, which a
    // server without battle data does not have, so this is the one place that can say it.
    if cli.debug_limit_full && hosted.battle.is_none() {
        warn!("--debug-limit-full has no effect: this server has no battle data, so battles are unsupported");
    }
    if cli.debug_status.is_some() && hosted.battle.is_none() {
        warn!("--debug-status has no effect: this server has no battle data, so battles are unsupported");
    }
    if (cli.debug_bonus_exp.is_some() || cli.debug_bonus_limit.is_some()) && hosted.battle.is_none() {
        warn!("--debug-bonus-exp / --debug-bonus-limit have no effect: this server has no battle data, so battles are unsupported");
    }
    if cli.debug_scene_preview && hosted.scenes.is_none() {
        warn!("--debug-scene-preview has no effect: this game has no story scenes");
    }
    if cli.debug_scene_preview && hosted.scenes.is_some() {
        warn!(
            "DEBUG: --debug-scene-preview is on: any player may put itself in a story scene's field at the scene's own story moment, on a made-up story. \
             Nothing of that story is ever saved or touches the account's real one (a smoke-test and QA aid; leave it off on a server real players use)"
        );
    }
    let config = cli.with_limits(ServerConfig {
        bind: cli.bind,
        area_key: hosted.area,
        state_dir,
        allow_registration: !cli.no_registration,
        tick_hz: cli.tick_hz,
        max_connections: cli.max_connections as usize,
        build_suffix: hosted.build_suffix,
        battle: hosted.battle,
        debug_encounters: cli.debug_encounters,
        scenes: hosted.scenes,
        ..ServerConfig::default()
    });
    let shutdown = shutdown_signal().map_err(|e| format!("cannot install signal handlers: {e}"))?;
    fmv_server::run(config, hosted.adapter, shutdown).await.map_err(|e| e.to_string())
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    init_tracing();
    match real_main(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("fmv-server").chain(args.iter().copied()))
    }

    #[tokio::test]
    async fn the_fixture_adapter_is_the_default_and_keeps_its_plain_build() {
        let hosted = build_adapter(&parse(&[]).unwrap()).await.expect("fixture adapter");
        assert!(hosted.adapter.game().is_fixture());
        assert_eq!(hosted.area, FIXTURE_AREA_KEY);
        assert_eq!(hosted.build_suffix, None);
        let hosted = build_adapter(&parse(&["--area", "other"]).unwrap()).await.expect("fixture adapter");
        assert_eq!(hosted.area, "other", "an explicit --area is passed through");
    }

    #[tokio::test]
    async fn ff7_without_content_fails_before_anything_is_bound_and_points_at_fmv_import() {
        // Hermetic: an explicit, empty state directory (never the owner's real library).
        let empty = tempfile::tempdir().unwrap();
        let lib = empty.path().to_str().unwrap();
        for args in [
            vec!["--game", "ff7", "--ff7-library", lib],
            vec!["--ff7-library", lib],
            vec!["--game", "fixture", "--ff7-library", lib], // an FF7 source flag always means FF7
            vec!["--ff7-library", lib, "--ff7-set", "ff7-pc-nope-00000000"],
            vec!["--ff7-flevel", "/nonexistent/flevel.lgp"],
        ] {
            let err = build_adapter(&parse(&args).unwrap()).await.err().unwrap_or_else(|| panic!("{args:?} must fail"));
            let text = err.to_string();
            assert!(!text.is_empty(), "{args:?}");
            if args.contains(&"--ff7-flevel") {
                assert!(text.contains("flevel.lgp"), "{args:?}: {text}");
            } else if !args.contains(&"--ff7-set") {
                assert!(text.contains("needs owned FF7 content imported via fmv-import"), "{args:?}: {text}");
            }
        }
    }

    /// `--debug-status` takes the names of the statuses a battle plays (any case, spaces and hyphens ignored), refuses the
    /// ones it does not, and refuses a set a battle cannot start with; like the other debug flags it needs
    /// `--debug-encounters`.
    #[test]
    fn debug_status_takes_names_of_statuses_a_battle_plays_and_needs_debug_encounters() {
        use fmv_ff7_battle::status::{Status, StatusSet};
        let set = |list: &[Status]| list.iter().copied().collect::<StatusSet>();
        let ok = |list: &str| parse(&["--debug-encounters", "--debug-status", list]).map(|c| c.debug_status);
        assert_eq!(ok("poison").unwrap(), Some(set(&[Status::Poison])));
        assert_eq!(ok("Poison, SILENCE ,m-barrier").unwrap(), Some(set(&[Status::Poison, Status::Silence, Status::MBarrier])));
        assert_eq!(ok("sleep,sleep").unwrap(), Some(set(&[Status::Sleep])), "twice is once");
        assert_eq!(parse(&["--debug-encounters"]).unwrap().debug_status, None, "off unless asked for");
        for (bad, says) in [
            ("confusion", "not a status a battle plays"),
            ("frog", "not a status a battle plays"),
            ("death", "not a status a battle plays"),
            ("near-death", "not a status a battle plays"),
            ("poison,nonsense", "nonsense"),
            ("", "names no status"),
            (",", "names no status"),
            ("haste,slow", "cannot all start"),
            ("sadness,fury,poison", "cannot all start"),
        ] {
            let err = ok(bad).err().unwrap_or_else(|| panic!("{bad:?} must be refused")).to_string();
            assert!(err.contains(says), "{bad:?}: {err}");
        }
        // The list of the ones it plays is in the error, so that the operator can pick.
        let err = ok("frog").err().unwrap().to_string();
        assert!(err.contains("Poison") && err.contains("Haste"), "{err}");
        // A debug aid never runs on its own.
        let err = parse(&["--debug-status", "poison"]).expect_err("needs --debug-encounters").to_string();
        assert!(err.contains("--debug-encounters"), "{err}");
        // It reaches the battle settings as the set.
        let cli = parse(&["--debug-encounters", "--debug-status", "regen,haste"]).unwrap();
        let settings = cli.battle_settings(std::sync::Arc::new(fmv_protocol::SyntheticRules::default()));
        assert_eq!(settings.debug_status, set(&[Status::Haste, Status::Regen]));
        assert!(parse(&["--debug-encounters"]).unwrap().battle_settings(std::sync::Arc::new(fmv_protocol::SyntheticRules::default())).debug_status.is_empty());
    }

    #[test]
    fn ff7_source_flags_select_the_game_and_the_source() {
        let cli = parse(&[]).unwrap();
        assert_eq!(cli.effective_game(), GameKind::Fixture);
        assert_eq!(parse(&["--game", "ff7"]).unwrap().effective_game(), GameKind::Ff7);
        for flags in [vec!["--ff7-library", "/x"], vec!["--ff7-set", "s"], vec!["--ff7-flevel", "/y/flevel.lgp"]] {
            assert_eq!(parse(&flags).unwrap().effective_game(), GameKind::Ff7, "{flags:?}");
        }

        // Plain --game ff7: the default library, no particular set.
        assert_eq!(parse(&["--game", "ff7"]).unwrap().ff7_source(), Ff7Source::Library { state_dir: None, set: None });
        let cli = parse(&["--ff7-library", "/state", "--ff7-set", "ff7-x"]).unwrap();
        assert_eq!(
            cli.ff7_source(),
            Ff7Source::Library { state_dir: Some(PathBuf::from("/state")), set: Some("ff7-x".into()) }
        );

        // Direct archive: labels default to `unknown` and can be given one by one.
        let cli = parse(&["--ff7-flevel", "/y/flevel.lgp", "--ff7-edition", "Steam", "--ff7-revision", "r1"]).unwrap();
        match cli.ff7_source() {
            Ff7Source::Flevel { path, labels } => {
                assert_eq!(path, Path::new("/y/flevel.lgp"));
                assert_eq!(
                    labels,
                    Ff7Labels { edition: "Steam".into(), region: "unknown".into(), language: "unknown".into(), revision: "r1".into() }
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_account_store_directory_follows_the_flags_and_the_environment() {
        use fmv_library::resolve_state_dir_from;
        let base = |env: Option<&str>, data: Option<&str>| {
            let (env, data) = (env.map(Into::into), data.map(PathBuf::from));
            move || resolve_state_dir_from(None, env, data)
        };
        // Default: server/ inside the base (the environment wins over the user data dir).
        assert_eq!(server_state_dir(None, false, base(Some("/env/state"), Some("/data"))).unwrap(), Some(PathBuf::from("/env/state/server")));
        assert_eq!(server_state_dir(None, false, base(None, Some("/data"))).unwrap(), Some(PathBuf::from("/data/ff-multiverse/server")));
        // --state-dir is the directory itself, whatever the environment says.
        assert_eq!(server_state_dir(Some(Path::new("/x")), false, base(Some("/env"), None)).unwrap(), Some(PathBuf::from("/x")));
        // --ephemeral keeps nothing and does not even look at the environment.
        assert_eq!(server_state_dir(None, true, || panic!("not consulted")).unwrap(), None);
        // No base at all is a clear error that names the way out.
        let err = server_state_dir(None, false, base(None, None)).unwrap_err();
        assert!(err.contains("--state-dir") && err.contains("--ephemeral"), "{err}");
    }

    #[test]
    fn account_flags_parse_and_conflict_sensibly() {
        let cli = parse(&[]).unwrap();
        assert!(cli.state_dir.is_none() && !cli.ephemeral && !cli.no_registration);
        let cli = parse(&["--state-dir", "/s", "--no-registration"]).unwrap();
        assert_eq!(cli.state_dir.as_deref(), Some(Path::new("/s")));
        assert!(cli.no_registration);
        assert!(parse(&["--ephemeral"]).unwrap().ephemeral);
        assert!(parse(&["--ephemeral", "--state-dir", "/s"]).is_err());
    }

    #[test]
    fn contradictory_ff7_flags_are_usage_errors() {
        // A library and a direct archive are two different sources.
        assert!(parse(&["--ff7-library", "/x", "--ff7-flevel", "/y"]).is_err());
        assert!(parse(&["--ff7-set", "s", "--ff7-flevel", "/y"]).is_err());
        // Labels only make sense for a direct archive.
        for label in ["--ff7-edition", "--ff7-region", "--ff7-language", "--ff7-revision"] {
            assert!(parse(&[label, "x"]).is_err(), "{label} needs --ff7-flevel");
            assert!(parse(&["--ff7-library", "/x", label, "x"]).is_err(), "{label} needs --ff7-flevel");
            assert!(parse(&["--ff7-flevel", "/y", label, "x"]).is_ok(), "{label}");
        }
    }

    #[test]
    fn the_fixture_rooms_are_a_synthetic_game_with_several_areas() {
        let cli = parse(&["--game", "fixture-rooms"]).unwrap();
        assert_eq!(cli.effective_game(), GameKind::FixtureRooms);
        // Real content asked for wins over it, like for the plain fixture.
        assert_eq!(parse(&["--game", "fixture-rooms", "--ff7-library", "/x"]).unwrap().effective_game(), GameKind::Ff7);
        let hosted = tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(build_adapter(&cli)).unwrap();
        assert_eq!(hosted.area, "hall", "the entry area of the rooms");
        assert!(hosted.battle.is_none() && hosted.build_suffix.is_none());
        let areas: Vec<String> = hosted.adapter.list_areas().into_iter().map(|a| a.id.key).collect();
        assert_eq!(areas, ["hall", "yard"]);
        assert!(hosted.adapter.list_areas().iter().all(|a| a.fixture), "every one of them is labelled FIXTURE");
        assert_eq!(parse(&["--game", "fixture-rooms", "--area", "yard"]).unwrap().area.as_deref(), Some("yard"));
    }

    #[test]
    fn cli_defaults_match_the_spec() {
        let cli = parse(&[]).unwrap();
        assert_eq!(cli.bind, SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), DEFAULT_DEV_PORT));
        assert_eq!(cli.game, GameKind::Fixture);
        assert_eq!(cli.area, None, "the default area depends on the game");
        assert_eq!(cli.tick_hz, 20);
        assert_eq!(cli.max_connections, 64);
        assert!(cli.ff7_library.is_none() && cli.ff7_set.is_none() && cli.ff7_flevel.is_none());
        let cli = parse(&["--bind", "127.0.0.1:0", "--game", "ff7", "--ff7-library", "/x"]).unwrap();
        assert_eq!(cli.bind.port(), 0);
        assert_eq!(cli.game, GameKind::Ff7);
        assert!(parse(&["--tick-hz", "0"]).is_err());
        assert!(parse(&["--max-connections", "0"]).is_err());
    }

    #[test]
    fn battle_flags_default_to_a_normal_server_and_map_into_the_settings() {
        use fmv_protocol::SyntheticRules;
        let cli = parse(&[]).unwrap();
        assert!(!cli.debug_encounters, "debug encounters are OFF unless asked for");
        assert!(!cli.no_random_encounters);
        assert_eq!((cli.battle_speed, cli.battle_tick_rate), (DEFAULT_BATTLE_SPEED, DEFAULT_TICKS_PER_SECOND));
        assert_eq!((cli.battle_level, cli.battle_seed, cli.debug_formation), (None, None, None));
        let defaults = cli.battle_settings(std::sync::Arc::new(SyntheticRules::default()));
        assert!(defaults.random_encounters && defaults.level.is_none() && defaults.fixed_seed.is_none());
        assert_eq!((defaults.battle_speed, defaults.ticks_per_second), (DEFAULT_BATTLE_SPEED, DEFAULT_TICKS_PER_SECOND));
        // The companions let the players go first unless told otherwise; 0 turns that off.
        assert_eq!(defaults.companion_pacing, fmv_server::companion::Pacing::default());
        assert_eq!(fmv_server::companion::Pacing::default(), fmv_server::companion::Pacing { turn_wait_ms: 5_000, max_wait_ms: 10_000 });
        let paced = parse(&["--companion-turn-wait-ms", "2500", "--companion-max-wait-ms", "0"]).unwrap();
        let paced = paced.battle_settings(std::sync::Arc::new(SyntheticRules::default())).companion_pacing;
        assert_eq!(paced, fmv_server::companion::Pacing { turn_wait_ms: 2_500, max_wait_ms: 0 });

        let cli = parse(&[
            "--debug-encounters",
            "--no-random-encounters",
            "--battle-speed",
            "0",
            "--battle-tick-rate",
            "240",
            "--battle-level",
            "7",
            "--battle-seed",
            "9",
            "--debug-formation",
            "301",
        ])
        .unwrap();
        assert!(cli.debug_encounters);
        let s = cli.battle_settings(std::sync::Arc::new(SyntheticRules::default()));
        assert_eq!((s.battle_speed, s.ticks_per_second, s.level), (0, 240, Some(7)));
        assert_eq!((s.fixed_seed, s.debug_formation, s.random_encounters), (Some(9), Some(301), false));
        // The settings the flags make are ones the server accepts.
        ServerConfig { battle: Some(s), debug_encounters: true, ..ServerConfig::default() }.validate().unwrap();

        // Out-of-range values are usage errors, not panics later.
        for bad in [
            &["--battle-tick-rate", "0"][..],
            &["--battle-tick-rate", "1001"],
            &["--battle-level", "0"],
            &["--battle-level", "100"],
            &["--battle-speed", "256"],
            &["--debug-formation", "70000"],
            &["--battle-seed", "-1"],
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn debug_limit_full_is_off_by_default_needs_debug_encounters_and_maps_into_the_settings() {
        use fmv_protocol::SyntheticRules;
        let rules = || std::sync::Arc::new(SyntheticRules::default());
        let cli = parse(&["--debug-encounters"]).unwrap();
        assert!(!cli.debug_limit_full, "off unless asked for");
        assert!(!cli.battle_settings(rules()).debug_limit_full);

        let cli = parse(&["--debug-encounters", "--debug-limit-full"]).unwrap();
        assert!(cli.debug_limit_full);
        let s = cli.battle_settings(rules());
        assert!(s.debug_limit_full);
        ServerConfig { battle: Some(s), debug_encounters: true, ..ServerConfig::default() }.validate().unwrap();

        // A debug flag never runs alone: the parser refuses it without --debug-encounters (a usage error).
        assert!(parse(&["--debug-limit-full"]).is_err());
        assert!(parse(&["--no-random-encounters", "--debug-limit-full"]).is_err());

        // The help says what it does and what it leaves alone.
        use clap::CommandFactory;
        let help = Cli::command().render_long_help().to_string();
        let flat = help.split_whitespace().collect::<Vec<_>>().join(" ");
        for needle in ["--debug-limit-full", "FULL limit gauge", "refused unless --debug-encounters is given too", "never stored"] {
            assert!(flat.contains(needle), "--help must mention {needle:?}:\n{help}");
        }
        // And never the old rule (a spent gauge stored), which the flag no longer follows.
        for stale in ["stored as the gauge the member ended with", "save is the normal one", "Only the gauge a battle STARTS with changes"] {
            assert!(!flat.contains(stale), "--help must not describe the old save rule ({stale:?}):\n{help}");
        }
    }

    #[test]
    fn the_bonus_flags_are_off_by_default_need_debug_encounters_never_meet_the_full_gauge_flag_and_map_into_the_settings() {
        use fmv_protocol::SyntheticRules;
        let rules = || std::sync::Arc::new(SyntheticRules::default());
        let cli = parse(&["--debug-encounters"]).unwrap();
        assert_eq!((cli.debug_bonus_exp, cli.debug_bonus_limit), (None, None), "off unless asked for");
        let s = cli.battle_settings(rules());
        assert_eq!((s.debug_bonus_exp, s.debug_bonus_limit), (0, 0));

        let cli = parse(&["--debug-encounters", "--debug-bonus-exp", "900", "--debug-bonus-limit", "100"]).unwrap();
        let s = cli.battle_settings(rules());
        assert_eq!((s.debug_bonus_exp, s.debug_bonus_limit), (900, 100));
        ServerConfig { battle: Some(s), debug_encounters: true, ..ServerConfig::default() }.validate().unwrap();

        // A debug flag never runs alone: the parser refuses each one without --debug-encounters (a usage error).
        assert!(parse(&["--debug-bonus-exp", "900"]).is_err());
        assert!(parse(&["--debug-bonus-limit", "100"]).is_err());
        // Nor next to the flag whose early return would make the limit bonus do nothing, nor with nonsense numbers.
        assert!(parse(&["--debug-encounters", "--debug-limit-full", "--debug-bonus-limit", "100"]).is_err());
        assert!(parse(&["--debug-encounters", "--debug-limit-full", "--debug-bonus-exp", "900"]).is_ok(), "the experience bonus has no quarrel with it");
        for bad in [["--debug-bonus-exp", "0"], ["--debug-bonus-exp", "-5"], ["--debug-bonus-limit", "0"], ["--debug-bonus-limit", "256"]] {
            assert!(parse(&["--debug-encounters", bad[0], bad[1]]).is_err(), "{bad:?}");
        }

        // The help says what they do, where they may not run and that they store what they make up.
        use clap::CommandFactory;
        let help = Cli::command().render_long_help().to_string();
        let flat = help.split_whitespace().collect::<Vec<_>>().join(" ");
        for needle in [
            "--debug-bonus-exp",
            "--debug-bonus-limit",
            "refused on the DEFAULT state directory",
            "make up numbers and STORE them",
            "debug_bonus_exp=EXP",
        ] {
            assert!(flat.contains(needle), "--help must mention {needle:?}:\n{help}");
        }
    }

    /// The bonus flags store numbers nobody earned: they never run on the default state directory, only on one the
    /// operator named (or none at all).
    #[test]
    fn the_bonus_flags_refuse_the_default_state_directory() {
        let refused = |args: &[&str]| parse(args).unwrap().check_bonus_flags_state();
        let on = ["--debug-encounters", "--debug-bonus-exp", "900"];
        let err = refused(&on).expect_err("no --state-dir: the default (real) one");
        assert!(err.contains("--state-dir") && err.contains("--ephemeral") && err.contains("default state directory"), "{err}");
        assert!(refused(&["--debug-encounters", "--debug-bonus-limit", "100"]).is_err());
        // (An empty --state-dir is refused by the parser itself, and `server_state_dir` would fall back to the default for
        // one that got through, which the check does not count as a choice either.)
        assert!(parse(&["--debug-encounters", "--debug-bonus-exp", "900", "--state-dir", ""]).is_err());
        // A named directory, or none kept at all, is fine; and the check is silent about everything else.
        assert!(refused(&["--debug-encounters", "--debug-bonus-exp", "900", "--state-dir", "/tmp/throw-away"]).is_ok());
        assert!(refused(&["--debug-encounters", "--debug-bonus-limit", "100", "--ephemeral"]).is_ok());
        assert!(refused(&["--debug-encounters"]).is_ok());
        assert!(refused(&[]).is_ok());
        assert!(refused(&["--debug-encounters", "--debug-limit-full"]).is_ok(), "the full-gauge flag stores nothing");
    }

    #[test]
    fn the_limit_flags_default_to_the_servers_defaults_and_zero_lifts_them() {
        // The flags' defaults and the library's are one set of values, so they cannot drift.
        let defaults = ServerConfig::default();
        let made = parse(&[]).unwrap().with_limits(ServerConfig::default());
        assert_eq!(made.max_connections_per_ip, defaults.max_connections_per_ip);
        assert_eq!(made.registrations_per_ip, defaults.registrations_per_ip);
        assert_eq!(made.registrations_per_minute, defaults.registrations_per_minute);
        assert_eq!(made.max_accounts, defaults.max_accounts);
        assert_eq!(made.hello_timeout, defaults.hello_timeout);
        assert_eq!(made.idle_timeout, defaults.idle_timeout);
        assert_eq!(made.inactivity_timeout, defaults.inactivity_timeout);
        made.validate().unwrap();

        let cli = parse(&[
            "--max-connections-per-ip",
            "3",
            "--registrations-per-ip",
            "2",
            "--registrations-per-minute",
            "30",
            "--max-accounts",
            "500",
            "--hello-timeout-secs",
            "7",
            "--idle-timeout-secs",
            "12",
            "--inactivity-timeout-secs",
            "90",
        ])
        .unwrap();
        let made = cli.with_limits(ServerConfig::default());
        assert_eq!(made.max_connections_per_ip, Some(3));
        assert_eq!(made.registrations_per_ip, Some(2));
        assert_eq!(made.registrations_per_minute, 30);
        assert_eq!(made.max_accounts, 500);
        assert_eq!(made.hello_timeout, Duration::from_secs(7));
        assert_eq!(made.idle_timeout, Some(Duration::from_secs(12)));
        assert_eq!(made.inactivity_timeout, Some(Duration::from_secs(90)));
        made.validate().unwrap();

        // 0 lifts a limit (and `--inactivity-timeout-secs 0` is "never").
        let off = parse(&["--max-connections-per-ip", "0", "--registrations-per-ip", "0", "--inactivity-timeout-secs", "0"])
            .unwrap()
            .with_limits(ServerConfig::default());
        assert_eq!((off.max_connections_per_ip, off.registrations_per_ip, off.inactivity_timeout), (None, None, None));
        off.validate().unwrap();

        // Out-of-range values are usage errors, not panics later.
        for bad in [
            &["--registrations-per-minute", "0"][..],
            &["--max-accounts", "0"],
            &["--hello-timeout-secs", "0"],
            &["--max-connections-per-ip", "300"],
            &["--registrations-per-ip", "99999"],
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn idle_timeout_is_on_by_default_and_zero_turns_it_off() {
        // The flag's default and the library's default are one value, so they cannot drift.
        let cli = parse(&[]).unwrap();
        assert_eq!(idle_timeout(cli.idle_timeout_secs), ServerConfig::default().idle_timeout);
        assert_eq!(idle_timeout(cli.idle_timeout_secs), Some(DEFAULT_IDLE_TIMEOUT));
        let cli = parse(&["--idle-timeout-secs", "0"]).unwrap();
        assert_eq!(idle_timeout(cli.idle_timeout_secs), None);
        let cli = parse(&["--idle-timeout-secs", "5"]).unwrap();
        assert_eq!(idle_timeout(cli.idle_timeout_secs), Some(Duration::from_secs(5)));
    }
}
