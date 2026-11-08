//! `fmv-server`: authoritative FF Multiverse server (protocol v2).
//!
//! Thin wrapper over the `fmv_server` library: parse flags, pick the game adapter, install
//! signal handlers, run. Logs go to stderr; stdout carries exactly one line, the READY line.
//!
//! `--game ff7` hosts a real FF7 field (default `md1stin`) from the owner's own content: the
//! imported library (`--ff7-library`, `--ff7-set`) or a `flevel.lgp` read directly
//! (`--ff7-flevel`). Both are opened read-only, and no asset byte ever leaves this process.
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
use fmv_fixture::{FixtureAdapter, FIXTURE_AREA_KEY};
use fmv_protocol::DEFAULT_DEV_PORT;
use fmv_server::ff7::{self, Ff7Labels, Ff7Source, SharedFf7, DEFAULT_FF7_AREA};
use fmv_server::{ServerConfig, DEFAULT_IDLE_TIMEOUT, MAX_CONNECTIONS_LIMIT, MAX_TICK_HZ};
use tracing::info;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum GameKind {
    /// Synthetic FIXTURE area. NOT FF7 support.
    Fixture,
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
  accounts only until the server stops. --no-registration refuses new accounts.";

#[derive(Parser, Debug)]
#[command(name = "fmv-server", version, about = "Authoritative FF Multiverse server (protocol v2)", after_help = AFTER_HELP)]
struct Cli {
    /// Address to listen on. Port 0 picks a free port (reported on the READY line).
    #[arg(long, value_name = "ADDR", default_value_t = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), DEFAULT_DEV_PORT))]
    bind: SocketAddr,

    /// Which game adapter hosts the area.
    #[arg(long, value_enum, default_value_t = GameKind::Fixture)]
    game: GameKind,

    /// Key of the area to host (default: `plaza` for the FIXTURE game, `md1stin` for FF7).
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

    /// Disconnect joined clients that send no message for this many seconds (0 = never). Clients
    /// must send something, for example a Ping (the reference client pings every second), well
    /// inside this window. The default frees the slot and avatar of silent or vanished peers.
    #[arg(long, value_name = "SECS", default_value_t = DEFAULT_IDLE_TIMEOUT.as_secs())]
    idle_timeout_secs: u64,
}

/// `--idle-timeout-secs`: 0 disables the timeout.
fn idle_timeout(secs: u64) -> Option<Duration> {
    (secs > 0).then(|| Duration::from_secs(secs))
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
        }),
        GameKind::Ff7 => {
            let opened = ff7::open_ff7(&cli.ff7_source())?;
            info!(source = %opened.origin, "opened the owned FF7 field archive read-only");
            let report = ff7::prewarm(&opened.adapter).await?;
            info!(areas = report.areas, skipped = report.skipped, elapsed = ?report.elapsed, "indexed the FF7 field archive");
            Ok(Hosted {
                adapter: Box::new(SharedFf7(opened.adapter)),
                area: cli.area.clone().unwrap_or_else(|| DEFAULT_FF7_AREA.to_string()),
                build_suffix: Some(ff7::build_tag()),
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
    let hosted = build_adapter(&cli).await.map_err(|e| e.to_string())?;
    let state_dir = server_state_dir(cli.state_dir.as_deref(), cli.ephemeral, || fmv_library::resolve_state_dir(None))?;
    let config = ServerConfig {
        bind: cli.bind,
        area_key: hosted.area,
        state_dir,
        allow_registration: !cli.no_registration,
        tick_hz: cli.tick_hz,
        max_connections: cli.max_connections as usize,
        idle_timeout: idle_timeout(cli.idle_timeout_secs),
        build_suffix: hosted.build_suffix,
        ..ServerConfig::default()
    };
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
