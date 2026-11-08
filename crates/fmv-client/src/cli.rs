//! Command line interface, plus the small pure helpers behind it (tint parsing, name-hash tint,
//! server address normalisation, duration validation).

use clap::{Parser, ValueEnum};
use fmv_core::AvatarAppearance;
use fmv_protocol::DEFAULT_DEV_PORT;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

/// Process exit codes (documented in `--help`).
pub mod exit {
    /// Success.
    pub const OK: u8 = 0;
    /// Runtime failure: cannot connect, handshake failed, server rejected us, connection lost,
    /// window or GPU start-up failed.
    pub const FAILURE: u8 = 1;
    // Exit code 2 is reserved by clap for command-line usage errors.
    /// Local content is missing or does not match the server's area (CannotJoin was sent), or the
    /// requested game is not available locally.
    pub const CONTENT: u8 = 3;
}

const AFTER_HELP: &str = "\
EXIT CODES:
  0  success
  1  runtime failure (connect/handshake/server reject/connection lost/window or GPU start-up)
  2  command-line usage error
  3  local content missing or not matching the server's area, or the game is unavailable locally

CONTENT:
  Only the FIXTURE area (synthetic, NOT FF7) is available locally in this increment.
  --game ff7 needs owned FF7 content imported via fmv-import.

HEADLESS OUTPUT:
  stdout carries exactly one line:
    SMOKE name=<n> area=<AreaId> fixture=<bool> peers_seen=<n> updates=<n> rejected=<n> rtt_ms=<n|-1>
  <n> in name= is the display name percent-encoded: letters, digits and - . _ ~ are kept, every
  other byte becomes %XX (\"Red XIII\" prints as Red%20XIII), so the line always splits on spaces.
  A server that goes silent for 10 s is treated as gone (exit code 1, the summary is still printed).

WINDOW CONTROLS:
  WASD / arrow keys move, Esc quits.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BotKind {
    /// Walk a circle around the spawn point.
    Circle,
    /// Walk back and forth along a line through the spawn point.
    Line,
    /// Stand still.
    Idle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum GameChoice {
    /// Synthetic FIXTURE content. NOT FF7.
    Fixture,
    /// Needs owned FF7 content imported via fmv-import (not available in this increment).
    Ff7,
}

#[derive(Debug, Parser)]
#[command(
    name = "fmv-client",
    version,
    about = "FF Multiverse client: connect to a server, join its area and walk around with other players",
    after_help = AFTER_HELP
)]
pub struct Args {
    /// Server address as host:port (the port defaults to the development port when omitted).
    #[arg(long, value_name = "HOST:PORT", default_value = DEFAULT_SERVER, value_parser = normalize_server)]
    pub server: String,

    /// Display name shown to other players (required, at most 24 characters).
    #[arg(long, value_name = "NAME", value_parser = parse_name)]
    pub name: String,

    /// Player tint as rrggbb hex (default: derived from the name).
    #[arg(long, value_name = "RRGGBB", value_parser = parse_tint)]
    pub tint: Option<[u8; 3]>,

    /// Run without a window as a scripted bot and print one SMOKE summary line.
    #[arg(long)]
    pub headless: bool,

    /// Bot movement pattern (headless only).
    #[arg(long, value_enum, default_value_t = BotKind::Circle, requires = "headless")]
    pub bot: BotKind,

    /// How long the bot stays in the area, in seconds (headless only).
    #[arg(long, value_name = "SECS", default_value = "10", value_parser = parse_duration_secs, requires = "headless")]
    pub duration: Duration,

    /// Which game's content to use. Only `fixture` is available locally in this increment.
    #[arg(long, value_enum, default_value_t = GameChoice::Fixture)]
    pub game: GameChoice,
}

/// Default server for `--server`. Kept in sync with `DEFAULT_DEV_PORT` by a unit test.
const DEFAULT_SERVER: &str = "127.0.0.1:47770";

impl Args {
    /// The tint to send: the explicit `--tint`, or one derived from the name.
    pub fn effective_tint(&self) -> [u8; 3] {
        self.tint.unwrap_or_else(|| tint_from_name(&self.name))
    }
}

/// Parse `rrggbb` (an optional leading `#` is accepted).
pub fn parse_tint(s: &str) -> Result<[u8; 3], String> {
    let t = s.trim();
    let h = t.strip_prefix('#').unwrap_or(t);
    if h.len() != 6 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("`{s}` is not a tint: expected 6 hex digits like ff8800"));
    }
    // All six bytes are ASCII hex digits, so slicing on byte offsets is on char boundaries.
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).map_err(|e| e.to_string());
    Ok([byte(0)?, byte(2)?, byte(4)?])
}

/// Deterministic, random-looking, bright tint derived from a name hash (FNV-1a -> hue).
pub fn tint_from_name(name: &str) -> [u8; 3] {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in name.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    // Mix the high bits down so short, similar names still spread over the hue wheel.
    h ^= h >> 33;
    h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
    h ^= h >> 33;
    let hue = (h % 360) as f32;
    hsv_to_rgb(hue, 0.65, 0.95)
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [u8; 3] {
    let h6 = h.rem_euclid(360.0) / 60.0;
    let c = v * s;
    let x = c * (1.0 - ((h6 % 2.0) - 1.0).abs());
    let (r, g, b) = match h6 as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    let to_u8 = |f: f32| ((f + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    [to_u8(r), to_u8(g), to_u8(b)]
}

/// Validate and clean the display name (same rules the server applies).
pub fn parse_name(s: &str) -> Result<String, String> {
    AvatarAppearance::sanitize_name(s)
        .ok_or_else(|| "the display name is empty after removing control characters".to_string())
}

/// Seconds, finite and within a day. Fractions are allowed (`0.5`).
pub fn parse_duration_secs(s: &str) -> Result<Duration, String> {
    let v: f64 = s
        .trim()
        .parse()
        .map_err(|_| format!("`{s}` is not a number of seconds"))?;
    if !v.is_finite() || !(0.0..=86_400.0).contains(&v) {
        return Err(format!("`{s}` is out of range: expected 0..=86400 seconds"));
    }
    Ok(Duration::from_secs_f64(v))
}

/// Accept `host:port`, `ip:port`, `[v6]:port`, a bare ip or a bare host (default dev port).
pub fn normalize_server(input: &str) -> Result<String, String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("the server address is empty".into());
    }
    if s.parse::<SocketAddr>().is_ok() {
        return Ok(s.to_string());
    }
    if let Ok(ip) = s.parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, DEFAULT_DEV_PORT).to_string());
    }
    let bad_host = |h: &str| h.is_empty() || h.chars().any(|c| c.is_whitespace() || c == '/' || c == '@');
    match s.rsplit_once(':') {
        Some((host, port)) => {
            if bad_host(host) || host.contains(':') {
                return Err(format!("`{s}` is not a valid host:port"));
            }
            match port.parse::<u16>() {
                Ok(p) if p != 0 => Ok(s.to_string()),
                _ => Err(format!("`{s}` has an invalid port `{port}`")),
            }
        }
        None if bad_host(s) => Err(format!("`{s}` is not a valid host")),
        None => Ok(format!("{s}:{DEFAULT_DEV_PORT}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn tint_parses_plain_and_hash_prefixed_hex() {
        assert_eq!(parse_tint("ff8800"), Ok([0xff, 0x88, 0x00]));
        assert_eq!(parse_tint("#00AaFf"), Ok([0x00, 0xaa, 0xff]));
        assert_eq!(parse_tint("  123456 "), Ok([0x12, 0x34, 0x56]));
    }

    #[test]
    fn tint_rejects_bad_input_without_panicking() {
        for bad in [
            "",
            "fff",
            "ff88000",
            "gg0000",
            "ff 800",
            "#12345",
            "##ff880",
            "ééé",
            "ff88é0",
            "\u{0}\u{0}\u{0}\u{0}\u{0}\u{0}",
            "+f8800",
            "-f8800",
        ] {
            assert!(parse_tint(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn name_hash_tint_is_deterministic_bright_and_varied() {
        assert_eq!(tint_from_name("cloud"), tint_from_name("cloud"));
        let names = [
            "a", "b", "c", "bot1", "bot2", "cloud", "tifa", "barret", "aerith", "red xiii",
        ];
        let tints: std::collections::HashSet<[u8; 3]> = names.iter().map(|n| tint_from_name(n)).collect();
        assert!(
            tints.len() >= 8,
            "names should spread over many tints, got {}",
            tints.len()
        );
        for t in tints {
            assert!(t.iter().copied().max().unwrap() >= 200, "tint {t:?} should be bright");
        }
    }

    #[test]
    fn hsv_primaries() {
        assert_eq!(hsv_to_rgb(0.0, 1.0, 1.0), [255, 0, 0]);
        assert_eq!(hsv_to_rgb(120.0, 1.0, 1.0), [0, 255, 0]);
        assert_eq!(hsv_to_rgb(240.0, 1.0, 1.0), [0, 0, 255]);
        assert_eq!(hsv_to_rgb(359.9, 0.0, 0.5), [128, 128, 128]);
    }

    #[test]
    fn duration_validation() {
        assert_eq!(parse_duration_secs("10"), Ok(Duration::from_secs(10)));
        assert_eq!(parse_duration_secs("0.5"), Ok(Duration::from_millis(500)));
        assert_eq!(parse_duration_secs("0"), Ok(Duration::ZERO));
        for bad in ["-1", "nan", "inf", "1e300", "86401", "abc", ""] {
            assert!(parse_duration_secs(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn server_address_normalisation() {
        assert_eq!(normalize_server("127.0.0.1:47770").unwrap(), "127.0.0.1:47770");
        assert_eq!(normalize_server("example.test:9000").unwrap(), "example.test:9000");
        assert_eq!(
            normalize_server("example.test").unwrap(),
            format!("example.test:{DEFAULT_DEV_PORT}")
        );
        assert_eq!(
            normalize_server("10.0.0.5").unwrap(),
            format!("10.0.0.5:{DEFAULT_DEV_PORT}")
        );
        assert_eq!(normalize_server("[::1]:1234").unwrap(), "[::1]:1234");
        assert_eq!(normalize_server("::1").unwrap(), format!("[::1]:{DEFAULT_DEV_PORT}"));
        for bad in [
            "",
            "  ",
            "host:",
            "host:0",
            "host:99999",
            ":80",
            "a b:80",
            "http://x:80",
            "a:b:80",
        ] {
            assert!(normalize_server(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn default_server_matches_protocol_dev_port() {
        assert_eq!(DEFAULT_SERVER, format!("127.0.0.1:{DEFAULT_DEV_PORT}"));
    }

    #[test]
    fn cli_defaults_and_requirements() {
        let a = Args::try_parse_from(["fmv-client", "--name", "Cloud"]).unwrap();
        assert_eq!(a.server, DEFAULT_SERVER);
        assert_eq!(a.name, "Cloud");
        assert!(!a.headless);
        assert_eq!(a.bot, BotKind::Circle);
        assert_eq!(a.duration, Duration::from_secs(10));
        assert_eq!(a.game, GameChoice::Fixture);
        assert_eq!(a.effective_tint(), tint_from_name("Cloud"));

        let a = Args::try_parse_from([
            "fmv-client",
            "--name",
            "x",
            "--tint",
            "#102030",
            "--headless",
            "--bot",
            "line",
            "--duration",
            "2.5",
        ])
        .unwrap();
        assert_eq!(a.effective_tint(), [0x10, 0x20, 0x30]);
        assert_eq!(a.bot, BotKind::Line);
        assert_eq!(a.duration, Duration::from_millis(2500));

        // --name is required; --bot/--duration need --headless; bad values are usage errors.
        assert!(Args::try_parse_from(["fmv-client"]).is_err());
        assert!(Args::try_parse_from(["fmv-client", "--name", "x", "--bot", "idle"]).is_err());
        assert!(Args::try_parse_from(["fmv-client", "--name", "x", "--duration", "3"]).is_err());
        assert!(Args::try_parse_from(["fmv-client", "--name", "x", "--tint", "nothex"]).is_err());
        assert!(Args::try_parse_from(["fmv-client", "--name", "\u{7}\u{7}"]).is_err());
        assert!(Args::try_parse_from(["fmv-client", "--name", "x", "--game", "ff7"]).is_ok());
        assert!(Args::try_parse_from(["fmv-client", "--name", "x", "--game", "ff8"]).is_err());
    }
}
