//! Headless bot mode: no window, scripted movement, one SMOKE summary line on stdout.
//!
//! stdout carries exactly one line (the SMOKE summary); every notice and error goes to stderr,
//! including the one-line `party:` notes about party events and refusals.

use crate::battle::BattleBot;
use crate::bot::{Bot, PartyBot};
use crate::cli::{exit, Args};
use crate::interp::render_time;
use crate::session::Session;
use fmv_core::{AnimState, AreaId, EntityId, Pos};
use std::collections::BTreeMap;
use std::io::Write;
use std::time::{Duration, Instant};

/// Simulation tick. Intents still go out at most every 50 ms (20 Hz); the finer tick keeps the
/// send cadence close to 20 Hz despite scheduler jitter.
const TICK: Duration = Duration::from_millis(10);
/// Cap on a single simulated step so a stalled process cannot make the bot teleport.
const MAX_DT: f32 = 0.2;
/// How long to wait for the `Leave` message to be flushed before exiting.
const LEAVE_WAIT: Duration = Duration::from_secs(2);

/// Horizontal distance covered by one entity, for the stderr summary. The SMOKE line is a fixed
/// contract, and `updates` alone cannot show that anybody walked (a player also receives the
/// echo of its own moves), so this is how a run proves the bots really moved.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct Odometer {
    last: Option<Pos>,
    total: f32,
}

impl Odometer {
    fn feed(&mut self, p: Pos) {
        if let Some(last) = self.last {
            let d = last.dist_xy(p);
            if d.is_finite() {
                self.total += d;
            }
        }
        self.last = Some(p);
    }
}

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

/// The fields of the SMOKE line, in the order they are printed (`scripts/smoke.sh` parses the
/// line, so names and order are a contract: new fields are only ever appended).
#[derive(Debug, Clone)]
pub struct Smoke<'a> {
    /// Our display name (as the server shows it).
    pub name: &'a str,
    pub area: &'a AreaId,
    pub fixture: bool,
    pub peers_seen: usize,
    pub updates: u64,
    pub rejected: u64,
    pub rtt: Option<Duration>,
    /// The LARGEST party this client was in at any moment of the run (0: never in one). The peak
    /// and not the final size: two bots that finish a few milliseconds apart would otherwise race
    /// each other's departure.
    pub party: usize,
    /// Shared battles this client entered (`EncounterStart` received).
    pub battles: u64,
    /// Battles the server ended in victory.
    pub victories: u64,
}

/// `SMOKE name=<n> area=<AreaId> fixture=<bool> peers_seen=<n> updates=<n> rejected=<n> rtt_ms=<n|-1> party=<n> battles=<n> victories=<n>`
///
/// `<n>` in `name=` is the display name percent-encoded by [`encode_name`]; the other fields
/// never contain whitespace (the area comes from the local adapter's catalog, not from the
/// network). `battles=` and `victories=` were appended in protocol v3; the fields before them are
/// unchanged.
pub fn smoke_line(smoke: &Smoke<'_>) -> String {
    let Smoke { name, area, fixture, peers_seen, updates, rejected, rtt, party, battles, victories } = smoke;
    let rtt_ms = rtt.map_or(-1, |d| (d.as_secs_f64() * 1000.0).round() as i64);
    let name = encode_name(name);
    format!("SMOKE name={name} area={area} fixture={fixture} peers_seen={peers_seen} updates={updates} rejected={rejected} rtt_ms={rtt_ms} party={party} battles={battles} victories={victories}")
}

pub fn run(args: &Args, mut session: Session) -> u8 {
    let mut bot = Bot::new(
        args.bot,
        session.state.own.pos(),
        session.area.bounds(),
        session.area.max_speed(),
    );
    let mut party_bot = PartyBot::new(args.party_invite_first_peer, args.party_accept);
    let mut battle_bot = BattleBot::new(args.battle_auto, args.debug_encounters, args.battle_wait_party);
    eprintln!(
        "headless: joined {} as `{}` (entity {}), bot={:?} for {:.1} s",
        session.info.area_id,
        session.info.name,
        session.state.self_entity().0,
        args.bot,
        args.duration.as_secs_f64()
    );
    if args.party_invite_first_peer || args.party_accept {
        eprintln!(
            "headless: party behaviour: invite the first peer: {}, accept invites: {}",
            args.party_invite_first_peer, args.party_accept
        );
    }
    if battle_bot.is_active() {
        eprintln!(
            "headless: battle behaviour: attack when the gauge is full; ask for a debug encounter: {}{}",
            args.debug_encounters,
            if args.debug_encounters && args.battle_wait_party > 1 {
                format!(" (once leading a party of {})", args.battle_wait_party)
            } else {
                String::new()
            }
        );
    }

    let start = Instant::now();
    let mut last = start;
    let mut ended: Option<String> = None;
    let mut own_walk = Odometer::default();
    let mut peer_walks: BTreeMap<EntityId, Odometer> = BTreeMap::new();
    // What the world looked like when the last battle ended, to show that play went on afterwards.
    let mut after_battle: Option<(f32, u64)> = None;
    let mut battles_ended = 0;
    while start.elapsed() < args.duration {
        let tick_start = Instant::now();
        if let Some(reason) = session.pump() {
            ended = Some(reason);
            break;
        }
        session.log_party_notes();
        session.log_battle_notes();
        if session.state.battle.ended() > battles_ended {
            battles_ended = session.state.battle.ended();
            after_battle = Some((own_walk.total, session.state.updates()));
        }
        let now = session.now();
        let remotes = session.state.remote_views(render_time(now));
        for peer in &remotes {
            peer_walks.entry(peer.id).or_default().feed(peer.pose.pos);
        }
        // Party requests: what the bot flags ask for, and a resync if events were lost.
        let visible: Vec<EntityId> = remotes.iter().map(|p| p.id).collect();
        for cmd in party_bot.step(now, &session.state.party, &visible) {
            session.send_party(cmd);
        }
        session.flush_party_sync();
        // Battles: attack when our gauge is full, ask for a debug encounter (once) if told to.
        for cmd in battle_bot.step(now, &session.state.battle, &session.state.party) {
            session.send_battle(cmd);
        }
        let dt = (tick_start - last).as_secs_f32().min(MAX_DT);
        last = tick_start;
        // A player in a battle is frozen in the field: it neither walks nor sends movement.
        let frozen = session.state.battle.in_battle();
        if dt > 0.0 && !frozen {
            let step = bot.step(session.area.as_ref(), session.state.own.pos(), dt);
            let facing = step.facing.unwrap_or_else(|| session.state.own.facing());
            let anim = if step.moved { AnimState::Walk } else { AnimState::Idle };
            session.state.own.set_motion(step.pos, facing, anim);
        }
        own_walk.feed(session.state.own.pos());
        if !frozen {
            session.flush_intent();
        }
        if let Some(rest) = TICK.checked_sub(tick_start.elapsed()) {
            std::thread::sleep(rest);
        }
    }
    eprintln!(
        "headless: walked {:.0} units (bot pattern shrunk to nothing: {}); {} peer(s) seen moving {:.0} units in total",
        own_walk.total,
        bot.is_stuck(),
        peer_walks.len(),
        // `fold` from +0.0: an empty `Sum` of f32 is -0.0, which would print as "-0".
        peer_walks.values().fold(0.0_f32, |acc, o| acc + o.total)
    );

    // Leave first, then take a last look at what the network thread queued before it stopped.
    session.leave(LEAVE_WAIT);
    if ended.is_none() {
        ended = session.pump();
    }
    session.log_party_notes();
    session.log_battle_notes();
    eprintln!("headless: {}", session.state.party.summary());
    if battle_bot.is_active() || session.state.battle.entered() > 0 || session.state.battle.rejections() > 0 {
        eprintln!("headless: battles: {}", session.state.battle.summary());
        if let Some((walked, updates)) = after_battle {
            // The proof that the field went on: the player walked (or the server told us where
            // everyone stands, which it does at once when a battle ends) after the last battle.
            eprintln!(
                "headless: after the last battle: walked {:.0} units, {} entity update(s) seen",
                own_walk.total - walked,
                session.state.updates().saturating_sub(updates)
            );
        }
    }
    // A replay that disagreed with the server is a bug somewhere: the run fails loudly.
    if ended.is_none() && session.state.battle.divergences() > 0 {
        ended = Some(format!(
            "the battle replay diverged from the server {} time(s) (see the BATTLE DIVERGENCE lines above)",
            session.state.battle.divergences()
        ));
    }

    let line = smoke_line(&Smoke {
        name: &session.info.name,
        area: &session.info.area_id,
        fixture: session.info.fixture,
        peers_seen: session.state.peers_seen(),
        updates: session.state.updates(),
        rejected: session.state.rejected(),
        rtt: session.state.last_rtt(),
        party: session.state.party.peak_party_size(),
        battles: session.state.battle.entered(),
        victories: session.state.battle.victories(),
    });
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
    fn the_odometer_sums_horizontal_steps_and_ignores_garbage() {
        let mut o = Odometer::default();
        o.feed(Pos::new(0.0, 0.0, 0.0));
        o.feed(Pos::new(3.0, 4.0, 99.0)); // height does not count
        o.feed(Pos::new(3.0, 4.0, 0.0));
        o.feed(Pos::new(f32::NAN, 0.0, 0.0)); // a non-finite sample adds nothing
        assert_eq!(o.total, 5.0);
    }

    fn smoke<'a>(name: &'a str, area: &'a AreaId, fixture: bool, rtt: Option<Duration>) -> Smoke<'a> {
        Smoke { name, area, fixture, peers_seen: 2, updates: 31, rejected: 1, rtt, party: 3, battles: 1, victories: 1 }
    }

    #[test]
    fn smoke_line_has_the_documented_shape() {
        let area = AreaId {
            game: GameId::Fixture,
            key: "plaza".into(),
        };
        let l = smoke_line(&smoke("botA", &area, true, Some(Duration::from_micros(2_600))));
        assert_eq!(
            l,
            "SMOKE name=botA area=Fixture/plaza fixture=true peers_seen=2 updates=31 rejected=1 rtt_ms=3 party=3 battles=1 victories=1"
        );
        let l = smoke_line(&Smoke {
            name: "b",
            area: &area,
            fixture: false,
            peers_seen: 0,
            updates: 0,
            rejected: 0,
            rtt: None,
            party: 0,
            battles: 0,
            victories: 0,
        });
        assert_eq!(
            l,
            "SMOKE name=b area=Fixture/plaza fixture=false peers_seen=0 updates=0 rejected=0 rtt_ms=-1 party=0 battles=0 victories=0"
        );
        assert!(!l.contains('\n'));
    }

    /// `scripts/smoke.sh` parses the line by key, and new fields are only ever appended: the
    /// protocol-v2 keys keep their names and their order, and `battles` and `victories` (v3) come
    /// last.
    #[test]
    fn the_smoke_keys_keep_their_order_and_the_battle_fields_are_last() {
        let area = AreaId { game: GameId::Fixture, key: "plaza".into() };
        let l = smoke_line(&smoke("x", &area, true, None));
        let keys: Vec<&str> = l.split_whitespace().skip(1).map(|f| f.split_once('=').unwrap().0).collect();
        assert_eq!(
            keys,
            ["name", "area", "fixture", "peers_seen", "updates", "rejected", "rtt_ms", "party", "battles", "victories"]
        );
        assert!(l.starts_with("SMOKE "), "{l}");
        assert!(l.ends_with(" party=3 battles=1 victories=1"), "{l}");
        // A v2 parser that reads the first eight fields (and `party` among them) still works.
        assert!(l.contains(" rtt_ms=-1 party=3 "), "{l}");
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

            let line = smoke_line(&Smoke { name, area: &area, fixture: true, peers_seen: 1, updates: 2, rejected: 3, rtt: None, party: 1, battles: 0, victories: 0 });
            assert_eq!(line.lines().count(), 1, "{line:?}");
            assert_eq!(line.split_whitespace().count(), 11, "SMOKE + 10 fields: {line:?}");
            for field in line.split_whitespace().skip(1) {
                assert!(field.split_once('=').is_some(), "{field:?} in {line:?}");
            }
            let first = line.split_whitespace().nth(1).unwrap();
            assert_eq!(first, format!("name={enc}"));
        }
    }
}
