//! Headless bot mode: no window, scripted movement, one SMOKE summary line on stdout.
//!
//! stdout carries exactly one line (the SMOKE summary); every notice and error goes to stderr.

use crate::bot::Bot;
use crate::cli::{exit, Args};
use crate::session::Session;
use fmv_core::{AnimState, AreaId};
use std::io::Write;
use std::time::{Duration, Instant};

/// Simulation tick. Intents still go out at most every 50 ms (20 Hz); the finer tick keeps the
/// send cadence close to 20 Hz despite scheduler jitter.
const TICK: Duration = Duration::from_millis(10);
/// Cap on a single simulated step so a stalled process cannot make the bot teleport.
const MAX_DT: f32 = 0.2;
/// How long to wait for the `Leave` message to be flushed before exiting.
const LEAVE_WAIT: Duration = Duration::from_secs(2);

/// Make a display name safe as one `key=value` token: every byte outside the RFC 3986 unreserved
/// set (`A-Z a-z 0-9 - . _ ~`) becomes `%XX` (UTF-8 bytes, upper-case hex). Plain names such as
/// `botA` are unchanged; `Red XIII` becomes `Red%20XIII`. Spaces, `=`, `%` and every Unicode
/// separator (U+2028 splits lines in many parsers) are encoded, so the line always splits on
/// whitespace into exactly the documented fields, and the name decodes back unambiguously.
pub fn encode_name(name: &str) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(name.len());
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// `SMOKE name=<n> area=<AreaId> fixture=<bool> peers_seen=<n> updates=<n> rejected=<n> rtt_ms=<n|-1>`
///
/// `<n>` is the display name percent-encoded by [`encode_name`]; the other fields never contain
/// whitespace (the area comes from the local adapter's catalog, not from the network).
pub fn smoke_line(
    name: &str,
    area: &AreaId,
    fixture: bool,
    peers_seen: usize,
    updates: u64,
    rejected: u64,
    rtt: Option<Duration>,
) -> String {
    let rtt_ms = rtt.map_or(-1, |d| (d.as_secs_f64() * 1000.0).round() as i64);
    let name = encode_name(name);
    format!("SMOKE name={name} area={area} fixture={fixture} peers_seen={peers_seen} updates={updates} rejected={rejected} rtt_ms={rtt_ms}")
}

pub fn run(args: &Args, mut session: Session) -> u8 {
    let mut bot = Bot::new(
        args.bot,
        session.state.own.pos(),
        session.area.bounds(),
        session.area.max_speed(),
    );
    eprintln!(
        "headless: joined {} as `{}` (entity {}), bot={:?} for {:.1} s",
        session.info.area_id,
        session.info.name,
        session.state.self_entity().0,
        args.bot,
        args.duration.as_secs_f64()
    );

    let start = Instant::now();
    let mut last = start;
    let mut ended: Option<String> = None;
    while start.elapsed() < args.duration {
        let tick_start = Instant::now();
        if let Some(reason) = session.pump() {
            ended = Some(reason);
            break;
        }
        let dt = (tick_start - last).as_secs_f32().min(MAX_DT);
        last = tick_start;
        if dt > 0.0 {
            let step = bot.step(session.area.as_ref(), session.state.own.pos(), dt);
            let facing = step.facing.unwrap_or_else(|| session.state.own.facing());
            let anim = if step.moved { AnimState::Walk } else { AnimState::Idle };
            session.state.own.set_motion(step.pos, facing, anim);
        }
        session.flush_intent();
        if let Some(rest) = TICK.checked_sub(tick_start.elapsed()) {
            std::thread::sleep(rest);
        }
    }

    // Leave first, then take a last look at what the network thread queued before it stopped.
    session.leave(LEAVE_WAIT);
    if ended.is_none() {
        ended = session.pump();
    }

    let line = smoke_line(
        &session.info.name,
        &session.info.area_id,
        session.info.fixture,
        session.state.peers_seen(),
        session.state.updates(),
        session.state.rejected(),
        session.state.last_rtt(),
    );
    // Ignore write errors (e.g. a closed pipe): the exit code still tells the story.
    let _ = writeln!(std::io::stdout().lock(), "{line}");
    let _ = std::io::stdout().flush();

    match ended {
        None => exit::OK,
        Some(reason) => {
            eprintln!("error: {reason}");
            exit::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_core::GameId;

    #[test]
    fn smoke_line_has_the_documented_shape() {
        let area = AreaId {
            game: GameId::Fixture,
            key: "plaza".into(),
        };
        let l = smoke_line("botA", &area, true, 2, 31, 1, Some(Duration::from_micros(2_600)));
        assert_eq!(
            l,
            "SMOKE name=botA area=Fixture/plaza fixture=true peers_seen=2 updates=31 rejected=1 rtt_ms=3"
        );
        let l = smoke_line("b", &area, false, 0, 0, 0, None);
        assert_eq!(
            l,
            "SMOKE name=b area=Fixture/plaza fixture=false peers_seen=0 updates=0 rejected=0 rtt_ms=-1"
        );
        assert!(!l.contains('\n'));
    }

    /// Decode what `encode_name` produced (the inverse a harness would apply).
    fn decode(s: &str) -> String {
        let b = s.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'%' {
                let hex = std::str::from_utf8(&b[i + 1..i + 3]).unwrap();
                out.push(u8::from_str_radix(hex, 16).unwrap());
                i += 3;
            } else {
                out.push(b[i]);
                i += 1;
            }
        }
        String::from_utf8(out).unwrap()
    }

    /// A name with spaces, `=`, `%` or Unicode separators must not break the one-line,
    /// whitespace-separated key=value contract, and must decode back to the real name.
    #[test]
    fn awkward_names_stay_one_token_and_round_trip() {
        let area = AreaId {
            game: GameId::Fixture,
            key: "plaza".into(),
        };
        assert_eq!(encode_name("botA"), "botA");
        assert_eq!(encode_name("a-b.c_d~e"), "a-b.c_d~e");
        assert_eq!(encode_name("Red XIII"), "Red%20XIII");
        for name in [
            "Red XIII",
            "a=b c",
            "100%",
            "x\u{2028}y\u{2029}z",
            "tab\tsep",
            "nbsp\u{a0}x",
            "ideographic\u{3000}space",
            "Cl\u{f6}ud \u{30af}\u{30e9}\u{30a6}\u{30c9}",
            "\u{202e}rtl",
        ] {
            let enc = encode_name(name);
            assert!(
                enc.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~%".contains(&b)),
                "{name:?} -> {enc}"
            );
            assert_eq!(decode(&enc), name);

            let line = smoke_line(name, &area, true, 1, 2, 3, None);
            assert_eq!(line.lines().count(), 1, "{line:?}");
            assert_eq!(line.split_whitespace().count(), 8, "SMOKE + 7 fields: {line:?}");
            for field in line.split_whitespace().skip(1) {
                assert!(field.split_once('=').is_some(), "{field:?} in {line:?}");
            }
            let first = line.split_whitespace().nth(1).unwrap();
            assert_eq!(first, format!("name={enc}"));
        }
    }
}
