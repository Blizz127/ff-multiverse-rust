//! `fmv-server`: authoritative FF Multiverse server (protocol v1).
//!
//! Thin wrapper over the `fmv_server` library: parse flags, pick the game adapter, install
//! signal handlers, run. Logs go to stderr; stdout carries exactly one line, the READY line.

use std::io::IsTerminal;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, ValueEnum};
use fmv_adapter_api::GameAdapter;
use fmv_fixture::{FixtureAdapter, FIXTURE_AREA_KEY};
use fmv_protocol::DEFAULT_DEV_PORT;
use fmv_server::{ServerConfig, DEFAULT_IDLE_TIMEOUT, MAX_CONNECTIONS_LIMIT, MAX_TICK_HZ};

const FF7_NOT_WIRED: &str = "FF7 adapter not wired yet; requires owned FF7 content imported via fmv-import";

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum GameKind {
    /// Synthetic FIXTURE area. NOT FF7 support.
    Fixture,
    /// Real FF7 content from the owner's own imported library (not wired yet).
    Ff7,
}

#[derive(Parser, Debug)]
#[command(name = "fmv-server", version, about = "Authoritative FF Multiverse server (protocol v1)")]
struct Cli {
    /// Address to listen on. Port 0 picks a free port (reported on the READY line).
    #[arg(long, value_name = "ADDR", default_value_t = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), DEFAULT_DEV_PORT))]
    bind: SocketAddr,

    /// Which game adapter hosts the area.
    #[arg(long, value_enum, default_value_t = GameKind::Fixture)]
    game: GameKind,

    /// Key of the area to host.
    #[arg(long, value_name = "KEY", default_value = FIXTURE_AREA_KEY)]
    area: String,

    /// Broadcast ticks per second.
    #[arg(long, value_name = "HZ", default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=i64::from(MAX_TICK_HZ)))]
    tick_hz: u32,

    /// Owned FF7 library to read (accepted, but the FF7 adapter is not wired yet: this exits
    /// with an error).
    #[arg(long, value_name = "PATH")]
    ff7_library: Option<PathBuf>,

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

/// Why an adapter could not be set up. Any error type converts into it, so wiring a real adapter
/// can use `?`.
type SetupError = Box<dyn std::error::Error + Send + Sync>;

/// The single place where an adapter is chosen.
fn build_adapter(game: GameKind, ff7_library: Option<&Path>) -> Result<Box<dyn GameAdapter>, SetupError> {
    // `--ff7-library` always asks for the FF7 adapter, even next to `--game fixture`.
    let game = if ff7_library.is_some() { GameKind::Ff7 } else { game };
    match game {
        GameKind::Fixture => Ok(Box::new(FixtureAdapter::new())),
        GameKind::Ff7 => open_ff7(ff7_library),
    }
}

/// THE FF7 WIRING POINT: replace the body of this function (and add the `fmv-ff7` dependency to
/// this crate's manifest); nothing else in this file changes. Open the FF7 adapter over the
/// owned library, read-only, and return `Ok(Box::new(adapter))`. Errors of any type can be
/// returned with `?`; they are printed as `error: ...` and the process exits nonzero before
/// anything is bound or printed. `library` is `--ff7-library` (`None` for plain `--game ff7`).
fn open_ff7(library: Option<&Path>) -> Result<Box<dyn GameAdapter>, SetupError> {
    let _ = library;
    Err(FF7_NOT_WIRED.into())
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
    let adapter = build_adapter(cli.game, cli.ff7_library.as_deref()).map_err(|e| e.to_string())?;
    let config = ServerConfig {
        bind: cli.bind,
        area_key: cli.area,
        tick_hz: cli.tick_hz,
        max_connections: cli.max_connections as usize,
        idle_timeout: idle_timeout(cli.idle_timeout_secs),
        ..ServerConfig::default()
    };
    let shutdown = shutdown_signal().map_err(|e| format!("cannot install signal handlers: {e}"))?;
    fmv_server::run(config, adapter, shutdown).await.map_err(|e| e.to_string())
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

    #[test]
    fn only_the_fixture_adapter_is_wired() {
        let adapter = build_adapter(GameKind::Fixture, None).expect("fixture adapter");
        assert!(adapter.game().is_fixture());
        for (game, lib) in [
            (GameKind::Ff7, None),
            (GameKind::Ff7, Some(Path::new("/nonexistent"))),
            (GameKind::Fixture, Some(Path::new("/nonexistent"))),
        ] {
            let err = build_adapter(game, lib).err().expect("FF7 is not wired yet");
            assert_eq!(err.to_string(), FF7_NOT_WIRED);
        }
    }

    #[test]
    fn cli_defaults_match_the_spec() {
        let cli = Cli::try_parse_from(["fmv-server"]).unwrap();
        assert_eq!(cli.bind, SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), DEFAULT_DEV_PORT));
        assert_eq!(cli.game, GameKind::Fixture);
        assert_eq!(cli.area, FIXTURE_AREA_KEY);
        assert_eq!(cli.tick_hz, 20);
        assert_eq!(cli.max_connections, 64);
        assert!(cli.ff7_library.is_none());
        let cli = Cli::try_parse_from(["fmv-server", "--bind", "127.0.0.1:0", "--game", "ff7", "--ff7-library", "/x"]).unwrap();
        assert_eq!(cli.bind.port(), 0);
        assert_eq!(cli.game, GameKind::Ff7);
        assert!(Cli::try_parse_from(["fmv-server", "--tick-hz", "0"]).is_err());
        assert!(Cli::try_parse_from(["fmv-server", "--max-connections", "0"]).is_err());
    }

    #[test]
    fn idle_timeout_is_on_by_default_and_zero_turns_it_off() {
        // The flag's default and the library's default are one value, so they cannot drift.
        let cli = Cli::try_parse_from(["fmv-server"]).unwrap();
        assert_eq!(idle_timeout(cli.idle_timeout_secs), ServerConfig::default().idle_timeout);
        assert_eq!(idle_timeout(cli.idle_timeout_secs), Some(DEFAULT_IDLE_TIMEOUT));
        let cli = Cli::try_parse_from(["fmv-server", "--idle-timeout-secs", "0"]).unwrap();
        assert_eq!(idle_timeout(cli.idle_timeout_secs), None);
        let cli = Cli::try_parse_from(["fmv-server", "--idle-timeout-secs", "5"]).unwrap();
        assert_eq!(idle_timeout(cli.idle_timeout_secs), Some(Duration::from_secs(5)));
    }
}
