//! Headless bot mode: no window, scripted movement, one SMOKE summary line on stdout.
//!
//! stdout carries exactly one line (the SMOKE summary); every notice and error goes to stderr,
//! including the one-line `party:` notes about party events and refusals. The one exception is a run with a `--scene-*`
//! flag: its `scene ...` lines (see `crate::scene::bot`) are printed on stdout, flushed, BEFORE the SMOKE line, which is
//! still the last one.
//!
//! What a run says about who its players are goes to stderr too, one `key=value` line each (see the
//! help's HEADLESS OUTPUT section): `character:` once after joining (what the server holds of this
//! player's own character: name, colour, body, binding, combat slot), `headless: peer` when another player
//! appears or changes (its name, colour and binding), `headless: npc` for each non-player character of the
//! area (what a smoke test checks the canon characters are still in the field with). Every value that came
//! from the network is percent-encoded by [`encode_name`], so a line always splits on spaces.

use crate::audio::GameAudio;
use crate::battle::BattleBot;
use crate::bot::{Bot, PartyBot, TravelBot, TravelGate};
use crate::cli::{exit, Args};
use crate::interp::render_time;
use crate::progress::slot_name;
use crate::scene::{Act, SceneBot, SceneBotConfig, Senses};
use crate::session::Session;
use crate::state::PeerSighting;
use crate::story::StoryBot;
use crate::talkbot::TalkBot;
use crate::ui::dialogue::ff7::Ff7Conversations;
use crate::ui::palette;
use fmv_core::{AnimState, AreaId, EntityId, Pos};
use fmv_ff7::Ff7Adapter;
use fmv_protocol::CharacterCard;
use std::collections::BTreeMap;
use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Simulation tick. Intents still go out at most every 50 ms (20 Hz); the finer tick keeps the
/// send cadence close to 20 Hz despite scheduler jitter.
const TICK: Duration = Duration::from_millis(10);
/// Cap on a single simulated step so a stalled process cannot make the bot teleport.
const MAX_DT: f32 = 0.2;
/// How long to wait for the `Leave` message to be flushed before exiting.
const LEAVE_WAIT: Duration = Duration::from_secs(2);
/// A bot that plays battles (`--battle-auto`) does not walk out of one when its duration is up: it
/// plays the battle it is in to the end, for at most this much longer (a stalled server must not
/// hold it here forever). Without this a run could end in the middle of a battle, and `battles=`
/// would count one that never got a result.
const BATTLE_FINISH_GRACE: Duration = Duration::from_secs(12);

/// Is the run past its `duration` only because the battle it plays is still going?
fn finishing_a_battle(plays_battles: bool, in_battle: bool, elapsed: Duration, duration: Duration) -> bool {
    plays_battles && in_battle && elapsed < duration + BATTLE_FINISH_GRACE
}

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

/// `text` as one `key=value` token, `-` for none or nothing: [`encode_name`] of it.
fn token(text: Option<&str>) -> String {
    text.filter(|t| !t.is_empty()).map_or_else(|| "-".to_string(), encode_name)
}

/// The line that says what the server holds of our own character, printed once after joining:
/// `character: name=<enc> tint=rrggbb body=<enc|-> binding=<enc|-> slot=<n|-> slot_name=<enc|-> migrated=<bool>`.
/// `body` is the identity's body choice, `binding` how the game draws the character (the `appearance_ref` of its
/// entity), `slot` the combat slot it fights in with the slot's own name from this client's table of
/// characters; each is `-` when the server says none. Everything that came from the network is percent-encoded
/// ([`encode_name`]: `ff7/body/sturdy` prints as `ff7%2Fbody%2Fsturdy`).
pub fn character_line(card: &CharacterCard) -> String {
    format!(
        "character: name={} tint={} body={} binding={} slot={} slot_name={} migrated={}",
        token(Some(&card.name)),
        palette::hex(card.tint),
        token(card.body.as_deref()),
        token(card.binding.as_deref()),
        card.slot.map_or_else(|| "-".to_string(), |slot| slot.to_string()),
        token(card.slot.and_then(slot_name)),
        card.migrated
    )
}

/// The line that says another player appeared in the area or changed its colour or binding:
/// `headless: peer entity=<n> name=<enc> tint=rrggbb binding=<enc|-> area=<area id>`. Printed when the player is
/// first seen in an area and again whenever its colour or binding changes, never every frame: a smoke test reads
/// each player's binding (the placeholder body) from it.
pub fn peer_line(peer: &PeerSighting) -> String {
    format!(
        "headless: peer entity={} name={} tint={} binding={} area={}",
        peer.entity.0,
        token(Some(&peer.name)),
        palette::hex(peer.tint),
        token(peer.binding.as_deref()),
        peer.area
    )
}

/// The line that says one non-player character of the area: `headless: npc area=<area id> id=<n> name=<enc>
/// model=<enc>` (the model is the skeleton stem the area's data names, `-` for none). Printed for every character
/// whenever the area's set of characters changes.
pub fn npc_line(area: &AreaId, id: u32, name: &str, model: &str) -> String {
    format!("headless: npc area={area} id={id} name={} model={}", token(Some(name)), token(Some(model)))
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
    /// How many times the server moved this client on to another area (a transfer the client
    /// refused, and the way back, do not count). `area` is where it ended up and `peers_seen` is
    /// counted in that final area only.
    pub area_changes: u64,
    /// The level the server last said our character has (protocol v5), 0 if it never said (a server
    /// without battles, such as the FIXTURE one, sends no progress; 0 is not a level).
    pub level: u32,
    /// The total experience the server last said our character has, 0 if it never said.
    pub exp: u32,
    /// How many items the server last said our account carries, the counts of every stack added up
    /// (protocol v6), 0 if it never said (a server without battle data sends no inventory).
    pub items: u32,
}

/// `SMOKE name=<n> area=<AreaId> fixture=<bool> peers_seen=<n> updates=<n> rejected=<n> rtt_ms=<n|-1> party=<n> battles=<n> victories=<n> area_changes=<n> level=<n> exp=<n> items=<n>`
///
/// `<n>` in `name=` is the display name percent-encoded by [`encode_name`]; the other fields
/// never contain whitespace (the area comes from the local adapter's catalog, not from the
/// network). `battles=` and `victories=` were appended in protocol v3, `area_changes=` after
/// them (area travel), `level=` and `exp=` after that (character progress, protocol v5) and `items=`
/// last (the inventory, protocol v6: the total of all counts); the fields before are unchanged.
pub fn smoke_line(smoke: &Smoke<'_>) -> String {
    let Smoke { name, area, fixture, peers_seen, updates, rejected, rtt, party, battles, victories, area_changes, level, exp, items } = smoke;
    let rtt_ms = rtt.map_or(-1, |d| (d.as_secs_f64() * 1000.0).round() as i64);
    let name = encode_name(name);
    format!("SMOKE name={name} area={area} fixture={fixture} peers_seen={peers_seen} updates={updates} rejected={rejected} rtt_ms={rtt_ms} party={party} battles={battles} victories={victories} area_changes={area_changes} level={level} exp={exp} items={items}")
}

/// What the scene bot looks at, at session time `now` with `peers` other players in view.
fn scene_senses(session: &Session, now: f64, peers: usize) -> Senses<'_> {
    let (echoes, echo_at) = session.state.own_echo();
    Senses {
        now,
        npcs: session.state.npc_views(),
        npc_epoch: session.state.npc_epoch(),
        party_size: session.state.party.party_size(),
        peers_in_view: peers,
        unsent: session.state.own.has_unsent(),
        echoes,
        echo_at,
        rejected: session.state.rejected(),
    }
}

#[cfg(test)]
thread_local! {
    /// The scene lines `print_scene_lines` printed on this thread: a test reads them here (a real run has them on stdout).
    static SCENE_LINES: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The scene lines printed on this thread since the last call (tests only).
#[cfg(test)]
fn take_scene_lines() -> Vec<String> {
    SCENE_LINES.with(|lines| std::mem::take(&mut *lines.borrow_mut()))
}

/// Print the scene bot's lines on stdout, one per event, flushed (a smoke test reads them as they come).
fn print_scene_lines(lines: Vec<String>) {
    #[cfg(test)]
    SCENE_LINES.with(|sink| sink.borrow_mut().extend(lines.iter().cloned()));
    for line in lines {
        let mut out = std::io::stdout().lock();
        // Ignore write errors (e.g. a closed pipe): the exit code still tells the story.
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    }
}

/// `audio` is the run's sound (off for a bot unless `FMV_AUDIO` was set, see `main::start_audio`): it
/// follows the area and the battle each tick and logs what it plays.
pub fn run(args: &Args, mut session: Session, ff7: Option<Arc<Ff7Adapter>>, mut audio: GameAudio) -> u8 {
    let mut bot = Bot::new(
        args.bot,
        session.state.own.pos(),
        session.area.bounds(),
        session.area.max_speed(),
    );
    let mut party_bot = PartyBot::new(args.party_invite_first_peer, args.party_accept).with_accept_after_split(args.party_accept_after_split);
    // `--story-advance`: walk to the story point that would advance this account and stand on it. With an
    // invite to send it holds still until the invite is out (the invite must go out while the others are in view).
    let mut story_bot = StoryBot::new(args.story_advance, args.party_invite_first_peer).with_adapter(session.content());
    // `--travel`: walk through the exits to these areas; with a party to form first it waits for it.
    let mut travel_bot = TravelBot::new(args.travel.clone(), args.party_invite_first_peer || args.party_accept).following(args.travel_follow);
    let mut battle_bot = BattleBot::new(args.battle_auto, args.debug_encounters, args.battle_wait_party).with_script(args.battle_script.clone());
    // `--talk`: talk to these characters of the area through the window's own conversation code (this machine's
    // own copy of the field's scripts; without FF7 data nobody has anything to say).
    let mut talk_bot = TalkBot::new(&args.talk, Box::new(Ff7Conversations::new(ff7.clone())));
    // Story scenes are built from this machine's own FF7 data (a client that was given a builder already, a test's, keeps it).
    // `--scene-*`: play one without waiting for a picture and say what happened on stdout, one line per event.
    if ff7.is_some() {
        session.state.scene.set_builder(Box::new(crate::scene::Ff7Scenes::new(ff7)));
    }
    let mut scene_bot = SceneBot::new(SceneBotConfig {
        preview: args.scene_preview.clone(),
        watch: args.scene_watch,
        answer: args.scene_answer.map(usize::from),
        wait: args.scene_wait_secs,
        watchers: args.scene_watchers,
        invite_first_peer: args.party_invite_first_peer,
        accept: args.party_accept,
    });
    // A headless run has no picture to wait for and no keys: a scene that starts for it (one it asked for with `--scene-*`,
    // or one the server starts by itself when the player arrives somewhere) is played through at once.
    session.state.scene.set_instant(true);
    eprintln!(
        "headless: joined {} as `{}` (entity {}), bot={:?} for {:.1} s",
        session.info.area_id,
        session.info.name,
        session.state.self_entity().0,
        args.bot,
        args.duration.as_secs_f64()
    );
    // Once, after joining: what the server holds of this player's own character.
    eprintln!("{}", character_line(&session.info.card));
    if args.party_invite_first_peer || args.party_accept {
        eprintln!(
            "headless: party behaviour: invite the first peer: {}, accept invites: {}",
            args.party_invite_first_peer, args.party_accept
        );
    }
    if travel_bot.is_active() {
        eprintln!("headless: travel behaviour: walk through the exits to {}", args.travel.join(", then "));
    }
    if story_bot.is_requested() {
        eprintln!(
            "headless: story behaviour: walk to the story point that would advance this account and stand on it{}",
            if args.party_invite_first_peer { " (after the party invite is out)" } else { "" }
        );
    }
    if args.party_accept_after_split {
        eprintln!("headless: party behaviour: accept an invite only once a player seen before is out of view");
    }
    if scene_bot.is_active() {
        eprintln!(
            "headless: scene behaviour: {}; the run ends when the scene is over and control has been checked, or after {:.0} s without progress (--duration does not apply)",
            match (&args.scene_preview, args.scene_watch) {
                (Some(id), false) => format!("ask for a preview of {}, press OK next to its trigger{}, play it and check control afterwards", crate::scene::clean_id(id), if args.scene_watchers > 0 { format!(" once {} other player(s) are in view", args.scene_watchers) } else { String::new() }),
                (Some(id), true) => format!("ask for a preview of {}, then watch the scene that starts for this player", crate::scene::clean_id(id)),
                (None, _) => "watch the scene that starts for this player".to_string(),
            },
            args.scene_wait_secs.as_secs_f64()
        );
    }
    if talk_bot.is_active() {
        eprintln!("headless: talk behaviour: talk once to {}, as soon as the area announces them, read with this player's own story", args.talk.join(", "));
    }
    if battle_bot.is_active() {
        let script = battle_bot.script_steps();
        eprintln!(
            "headless: battle behaviour: when the ATB gauge is full, heal with the cheapest spell if HP is low, else give the limit break if the limit gauge is full, else attack; {}; ask for a debug encounter: {}{}",
            if script.is_empty() { "never escape".to_string() } else { "escape only where the script says so".to_string() },
            args.debug_encounters,
            if args.debug_encounters && args.battle_wait_party > 1 {
                format!(" (once leading a party of {})", args.battle_wait_party)
            } else {
                String::new()
            }
        );
        if !script.is_empty() {
            eprintln!(
                "headless: battle script given: {} entries, used first, one per full gauge: {}",
                script.len(),
                script.iter().map(ToString::to_string).collect::<Vec<_>>().join(",")
            );
        }
    }

    let start = Instant::now();
    let mut last = start;
    let mut ended: Option<String> = None;
    let mut own_walk = Odometer::default();
    let mut peer_walks: BTreeMap<EntityId, Odometer> = BTreeMap::new();
    // What the world looked like when the last battle ended, to show that play went on afterwards.
    let mut after_battle: Option<(f32, u64)> = None;
    let mut battles_ended = 0;
    // The area's NPCs as last reported: id, name and model (the snapshot of an area says who they are).
    let mut last_npcs: Vec<(u32, String, String)> = Vec::new();
    // How many other players were in view at the last look (`usize::MAX`: none yet).
    let mut last_in_view = usize::MAX;
    // The version of the area's NPC list the story bot was last told (`u64::MAX`: none yet).
    let mut last_npc_epoch = u64::MAX;
    // OK presses sent (ladders, lift panels, talks), for the summary.
    let mut presses_sent = 0_u32;
    while (if scene_bot.is_active() { !scene_bot.is_done() } else { start.elapsed() < args.duration })
        || finishing_a_battle(battle_bot.is_active(), session.state.battle.in_battle(), start.elapsed(), args.duration)
    {
        let tick_start = Instant::now();
        if let Some(reason) = session.pump() {
            ended = Some(reason);
            break;
        }
        session.log_party_notes();
        session.log_battle_notes();
        session.log_progress_notes();
        session.log_story_notes();
        session.log_countdown_notes();
        for peer in session.state.drain_peer_sightings() {
            eprintln!("{}", peer_line(&peer));
        }
        // The server moved us to another area: walk the pattern of the new one, and cross it off
        // the route if that is where the bot meant to go.
        if let Some(change) = session.take_area_change() {
            bot = Bot::new(args.bot, session.state.own.pos(), session.area.bounds(), session.area.max_speed());
            let reached = travel_bot.arrived(&change.area.key, session.now());
            story_bot.arrived();
            last_npc_epoch = u64::MAX;
            eprintln!(
                "headless: now in {} (area change {}){}",
                change.area,
                session.state.area_changes(),
                match (reached, travel_bot.next_area()) {
                    (true, Some(next)) => format!("; next stop: {next}"),
                    (true, None) => "; the route is done".to_string(),
                    (false, _) => String::new(),
                }
            );
            // The peers of the area we left are not seen any more: the counters of the pattern's
            // walk start again in the new one.
            peer_walks.clear();
        }
        // The music follows the area and the battle (a no-op for a silent bot).
        audio.update_from(&session);
        let npcs: Vec<(u32, String, String)> = session.state.npc_views().iter().map(|n| (n.id, n.name.clone(), n.model.clone())).collect();
        if npcs != last_npcs {
            if !npcs.is_empty() {
                let names: Vec<&str> = npcs.iter().map(|(_, name, _)| name.as_str()).collect();
                eprintln!("headless: {} NPC(s) in {}: {}", npcs.len(), session.info.area_id, names.join(", "));
                for (id, name, model) in &npcs {
                    eprintln!("{}", npc_line(&session.info.area_id, *id, name, model));
                }
            }
            last_npcs = npcs;
        }
        // The story bot goes to a character that a story point names once the area has announced it.
        if session.state.npc_epoch() != last_npc_epoch {
            last_npc_epoch = session.state.npc_epoch();
            story_bot.see_npcs(session.state.npc_views());
        }
        // Conversations: each wanted character is talked to once, when the area has announced it (and with the
        // story this player has reached by then).
        if !talk_bot.is_done() {
            for line in talk_bot.step_with(&session.info.area_id.key, session.state.npc_views(), session.state.story.state()) {
                eprintln!("{line}");
            }
        }
        if session.state.battle.ended() > battles_ended {
            battles_ended = session.state.battle.ended();
            after_battle = Some((own_walk.total, session.state.updates()));
        }
        let now = session.now();
        let remotes = session.state.remote_views(render_time(now));
        for peer in &remotes {
            peer_walks.entry(peer.id).or_default().feed(peer.pose.pos);
        }
        // How many other players this one can see, whenever it changes: a phase split shows as the count
        // falling to 0 while both are still connected, and a party that joins the phases as it rising again.
        if remotes.len() != last_in_view {
            eprintln!("headless: peers in view: {}", remotes.len());
            last_in_view = remotes.len();
        }
        // A bot that leads a party waits for the others to be in view before it walks onto a battle of the story.
        story_bot.see_party(session.state.party.party_size(), remotes.len());
        // Party requests: what the bot flags ask for, and a resync if events were lost.
        let visible: Vec<EntityId> = remotes.iter().map(|p| p.id).collect();
        for cmd in party_bot.step(now, &session.state.party, &visible) {
            session.send_party(cmd);
        }
        session.flush_party_sync();
        // Story scenes: a bot plays one through without waiting (it moves past every window at once), and the bot's script
        // around it (the preview, the press, the control check) is stepped here. What it printed goes to stdout, flushed.
        // (every headless run moves past the windows of a scene it is in: without it a scene the server started for a route
        // bot would hold the bot until the server's own timeout, and the story the scene carries would never move)
        session.state.scene.autoplay(if scene_bot.is_active() { scene_bot.answer() } else { args.scene_answer.map(usize::from) });
        for event in session.state.scene.take_events() {
            if scene_bot.is_active() {
                scene_bot.on_event(&event, now);
            }
        }
        if scene_bot.is_active() {
            let acts = scene_bot.step(&scene_senses(&session, now, remotes.len()));
            for act in acts {
                match act {
                    Act::Preview(id) => session.state.scene.request_preview(&id),
                    Act::Press(npc) => {
                        eprintln!("headless: interact area={} npc={npc} (a scene's trigger)", session.info.area_id);
                        presses_sent += 1;
                        session.send_interact(Some(npc));
                    }
                    Act::Probe => {
                        let target = crate::scene::pick_probe(session.area.as_ref(), session.state.own.pos());
                        scene_bot.probe_target(target, &scene_senses(&session, now, remotes.len()));
                        if let Some(to) = target {
                            let from = session.state.own.pos();
                            session.state.own.set_motion(to, (to.y - from.y).atan2(to.x - from.x), AnimState::Walk);
                        }
                    }
                }
            }
            print_scene_lines(scene_bot.take_lines());
        }
        session.flush_scene();
        session.log_scene_notes();
        // Battles: attack when our gauge is full, ask for a debug encounter (once) if told to.
        for cmd in battle_bot.step(now, &session.state.battle, &session.state.party) {
            session.send_battle(cmd);
        }
        for note in battle_bot.drain_script_notes() {
            eprintln!("battle: script: {note}");
        }
        let dt = (tick_start - last).as_secs_f32().min(MAX_DT);
        last = tick_start;
        // A player in a battle is frozen in the field: it neither walks nor sends movement. So is one held in a story scene,
        // and one that plays a scene stands still from the moment it asks for its preview (it must stand where its press is
        // judged, and its probe of control afterwards must be the only move it makes).
        let frozen = session.state.battle.in_battle() || session.state.scene.is_active() || scene_bot.holds_still();
        // Whoever moved the player, the bots of an area with links keep the arming of its walk-on lines from what
        // they see (a step is a step, a jump is the server putting the player somewhere).
        if story_bot.is_requested() {
            story_bot.observe(session.area.as_ref(), session.state.own.pos());
        }
        if travel_bot.is_active() {
            travel_bot.observe(session.area.as_ref(), session.state.own.pos());
        }
        // Which of the two bots decides now (see below): the one whose press, if it wants one, goes out.
        let mut story_decides = false;
        if dt > 0.0 && !frozen {
            // The story comes first: while there is a point in this area that would advance this player's story
            // the story bot walks to it (and stands still if it must wait for its invite); once there is none, a
            // bot that also travels goes on to the next exit. A bot that only advances its story stands still then.
            // (a bot that travels too tells the story bot where its route goes next: the lift's ride back is pursued when
            // it opens that way and the ride down is not when it would close it)
            story_bot.set_wanted_exit(if travel_bot.is_active() { travel_bot.next_area() } else { None });
            let story_walks = story_bot.wants_to_walk(session.area.as_ref(), session.state.story.progress(), session.state.own.pos());
            story_decides = story_bot.is_requested() && (story_walks || !travel_bot.is_active());
            let step = if story_decides {
                // The story bot decides every step (it stands still when there is nothing to advance).
                if story_bot.may_walk(now, session.state.party.outgoing_invite_seen()) {
                    let speed = session.area.max_speed() * crate::bot::BOT_SPEED_FACTOR;
                    story_bot.step(session.area.as_ref(), session.state.own.pos(), dt, speed, session.state.story.progress())
                } else {
                    crate::bot::BotStep { pos: session.state.own.pos(), facing: None, moved: false }
                }
            } else if travel_bot.is_active() {
                let gate = TravelGate {
                    now,
                    party_size: session.state.party.party_size(),
                    peers_seen: session.state.peers_seen(),
                    peers_in_view: remotes.len(),
                };
                if travel_bot.may_walk(gate) {
                    travel_bot.step(session.area.as_ref(), session.state.own.pos(), dt, session.area.max_speed() * crate::bot::BOT_SPEED_FACTOR)
                } else {
                    crate::bot::BotStep { pos: session.state.own.pos(), facing: None, moved: false }
                }
            } else {
                bot.step(session.area.as_ref(), session.state.own.pos(), dt)
            };
            let facing = step.facing.unwrap_or_else(|| session.state.own.facing());
            let anim = if step.moved { AnimState::Walk } else { AnimState::Idle };
            session.state.own.set_motion(step.pos, facing, anim);
        }
        own_walk.feed(session.state.own.pos());
        if !frozen || scene_bot.probing() {
            session.flush_intent();
        }
        if !frozen {
            // A press of OK (the elevator's panel, a ladder, a talk) is judged by the server where it holds the
            // player: it goes out once the intent that tells it where the bot stands has.
            let wants = if story_decides { story_bot.wants_press() } else { travel_bot.wants_press() };
            if wants && !session.state.own.has_unsent() {
                let press = if story_decides { story_bot.take_press() } else { travel_bot.take_press() };
                if let Some(press) = press {
                    presses_sent += 1;
                    eprintln!("headless: interact area={} npc={}", session.info.area_id, press.npc.map_or_else(|| "-".to_string(), |id| id.to_string()));
                    session.send_interact(press.npc);
                }
            }
        }
        for note in story_bot.drain_notes().into_iter().chain(travel_bot.drain_notes()) {
            eprintln!("headless: route: {note}");
        }
        if let Some(rest) = TICK.checked_sub(tick_start.elapsed()) {
            std::thread::sleep(rest);
        }
    }
    for line in talk_bot.finish() {
        eprintln!("{line}");
    }
    // A bot that was asked to play a scene and did not see it finish fails the run (the loop above only ends when the scene
    // bot is done, or the session did).
    print_scene_lines(scene_bot.take_lines());
    if scene_bot.is_active() && ended.is_none() {
        if let Some(why) = scene_bot.failure() {
            ended = Some(format!("scene: {why}"));
        } else if !scene_bot.is_done() {
            ended = Some("scene: the run ended before the scene did".to_string());
        }
    }
    if let Some(why) = story_bot.gave_up() {
        eprintln!("headless: story: gave up: {why}");
    }
    if story_bot.is_requested() || session.state.story.advances() > 0 {
        eprintln!("headless: story advances: {}", session.state.story.advances());
    }
    if session.state.countdown.told() > 0 {
        eprintln!(
            "headless: countdown: told {} time(s), {} at the end",
            session.state.countdown.told(),
            if session.state.countdown.is_running() { "still running" } else { "over" }
        );
    }
    if presses_sent > 0 {
        eprintln!("headless: OK presses sent: {presses_sent}");
    }
    if let Some(why) = travel_bot.gave_up() {
        eprintln!("headless: travel: gave up: {why}");
    } else if let Some(next) = travel_bot.next_area() {
        eprintln!("headless: travel: did not reach {next} in time");
    } else if !args.travel.is_empty() {
        eprintln!("headless: travel: the route {} was walked to the end", args.travel.join(", then "));
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
    session.log_progress_notes();
    session.log_story_notes();
    session.log_countdown_notes();
    for peer in session.state.drain_peer_sightings() {
        eprintln!("{}", peer_line(&peer));
    }
    eprintln!("headless: {}", session.state.party.summary());
    if let Some(summary) = audio.summary() {
        eprintln!("headless: audio: {summary}");
    }
    if battle_bot.is_active() || session.state.battle.entered() > 0 || session.state.battle.rejections() > 0 {
        eprintln!("headless: battles: {}", session.state.battle.summary());
        // How the script went (every entry is exactly one of applied, skipped, refused, lost or unused).
        let script = battle_bot.script_summary(&session.state.battle);
        for note in battle_bot.drain_script_notes() {
            eprintln!("battle: script: {note}");
        }
        if let Some(script) = script {
            eprintln!("headless: battle script: {script}");
        }
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
        area_changes: session.state.area_changes(),
        level: session.state.progress.level(),
        exp: session.state.progress.exp(),
        items: session.state.battle.inventory_total(),
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
    fn the_character_line_says_what_the_server_holds_in_percent_encoded_key_value_tokens() {
        let card = CharacterCard {
            name: "Red XIII".into(),
            tint: [0x5a, 0x7d, 0xf5],
            body: Some("sturdy".into()),
            binding: Some("ff7/body/sturdy".into()),
            slot: Some(4),
            migrated: false,
        };
        assert_eq!(
            character_line(&card),
            "character: name=Red%20XIII tint=5a7df5 body=sturdy binding=ff7%2Fbody%2Fsturdy slot=4 slot_name=Red%20XIII migrated=false"
        );
        // An account that predates the character creator: a canon look as its binding, no body, a slot that was its own.
        let migrated = CharacterCard { name: "Old".into(), tint: [1, 2, 3], body: None, binding: Some("ff7/cloud".into()), slot: Some(0), migrated: true };
        assert_eq!(character_line(&migrated), "character: name=Old tint=010203 body=- binding=ff7%2Fcloud slot=0 slot_name=Cloud migrated=true");
        // A game with no bodies and no slots (the FIXTURE), and a slot this client has no name for.
        assert_eq!(
            character_line(&CharacterCard::plain("bot", [255, 0, 16])),
            "character: name=bot tint=ff0010 body=- binding=- slot=- slot_name=- migrated=false"
        );
        let nobody = CharacterCard { slot: Some(200), ..CharacterCard::plain("x", [0, 0, 0]) };
        assert!(character_line(&nobody).ends_with(" slot=200 slot_name=- migrated=false"));
        // Whatever the server put in the card, the line stays one line of tokens that split on spaces.
        let hostile = CharacterCard {
            name: "a b=c\u{2028}d".into(),
            body: Some("x y".into()),
            binding: Some("p=q r".into()),
            ..CharacterCard::plain("", [0, 0, 0])
        };
        let line = character_line(&hostile);
        assert_eq!(line.lines().count(), 1, "{line:?}");
        let fields: Vec<&str> = line.split_whitespace().skip(1).collect();
        assert_eq!(fields.len(), 7, "{line:?}");
        assert!(fields.iter().all(|f| f.split_once('=').is_some_and(|(k, v)| !k.is_empty() && !v.is_empty())), "{line:?}");
        assert!(line.starts_with("character: name=a%20b%3Dc%E2%80%A8d tint=000000 body=x%20y binding=p%3Dq%20r "), "{line}");
    }

    #[test]
    fn the_peer_and_npc_lines_carry_ids_names_colours_and_bindings_as_tokens() {
        let area = AreaId { game: GameId::Ff7, key: "md1stin".into() };
        let peer = PeerSighting { entity: EntityId(8), name: "Wedge".into(), tint: [200, 10, 10], binding: Some("ff7/body/slim".into()), area: area.clone() };
        assert_eq!(peer_line(&peer), "headless: peer entity=8 name=Wedge tint=c80a0a binding=ff7%2Fbody%2Fslim area=Ff7/md1stin");
        let plain = PeerSighting { binding: None, name: "Red XIII".into(), ..peer };
        assert_eq!(peer_line(&plain), "headless: peer entity=8 name=Red%20XIII tint=c80a0a binding=- area=Ff7/md1stin");
        assert_eq!(npc_line(&area, 2, "Barret", "acgd"), "headless: npc area=Ff7/md1stin id=2 name=Barret model=acgd");
        assert_eq!(npc_line(&area, 0, "gu 0", ""), "headless: npc area=Ff7/md1stin id=0 name=gu%200 model=-");
    }

    #[test]
    fn the_odometer_sums_horizontal_steps_and_ignores_garbage() {
        let mut o = Odometer::default();
        o.feed(Pos::new(0.0, 0.0, 0.0));
        o.feed(Pos::new(3.0, 4.0, 99.0)); // height does not count
        o.feed(Pos::new(3.0, 4.0, 0.0));
        o.feed(Pos::new(f32::NAN, 0.0, 0.0)); // a non-finite sample adds nothing
        assert_eq!(o.total, 5.0);
    }

    #[test]
    fn a_battle_bot_finishes_the_battle_it_is_in_but_only_for_a_while() {
        let d = Duration::from_secs(40);
        let at = |s: u64| Duration::from_secs(s);
        // Within the duration the loop runs anyway; the rule only matters after it.
        assert!(finishing_a_battle(true, true, at(41), d), "a running battle is played out");
        assert!(finishing_a_battle(true, true, at(40 + 11), d));
        assert!(!finishing_a_battle(true, true, at(40 + 12), d), "but not forever");
        assert!(!finishing_a_battle(true, false, at(41), d), "no battle, nothing to finish");
        assert!(!finishing_a_battle(false, true, at(41), d), "a bot that does not play battles just stops");
    }

    fn smoke<'a>(name: &'a str, area: &'a AreaId, fixture: bool, rtt: Option<Duration>) -> Smoke<'a> {
        Smoke { name, area, fixture, peers_seen: 2, updates: 31, rejected: 1, rtt, party: 3, battles: 1, victories: 1, area_changes: 2, level: 7, exp: 626, items: 5 }
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
            "SMOKE name=botA area=Fixture/plaza fixture=true peers_seen=2 updates=31 rejected=1 rtt_ms=3 party=3 battles=1 victories=1 area_changes=2 level=7 exp=626 items=5"
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
            area_changes: 0,
            level: 0,
            exp: 0,
            items: 0,
        });
        assert_eq!(
            l,
            "SMOKE name=b area=Fixture/plaza fixture=false peers_seen=0 updates=0 rejected=0 rtt_ms=-1 party=0 battles=0 victories=0 area_changes=0 level=0 exp=0 items=0"
        );
        assert!(!l.contains('\n'));
    }

    /// `scripts/smoke.sh` parses the line by key, and new fields are only ever appended: the
    /// protocol-v2 keys keep their names and their order, `battles` and `victories` (v3) follow,
    /// `area_changes` (area travel) after them, `level` and `exp` (character progress) after those and
    /// `items` (the inventory, v6) comes last.
    #[test]
    fn the_smoke_keys_keep_their_order_and_the_newest_fields_are_last() {
        let area = AreaId { game: GameId::Fixture, key: "plaza".into() };
        let l = smoke_line(&smoke("x", &area, true, None));
        let keys: Vec<&str> = l.split_whitespace().skip(1).map(|f| f.split_once('=').unwrap().0).collect();
        assert_eq!(
            keys,
            ["name", "area", "fixture", "peers_seen", "updates", "rejected", "rtt_ms", "party", "battles", "victories", "area_changes", "level", "exp", "items"]
        );
        assert!(l.starts_with("SMOKE "), "{l}");
        assert!(l.ends_with(" party=3 battles=1 victories=1 area_changes=2 level=7 exp=626 items=5"), "{l}");
        // A v2 parser that reads the first eight fields (and `party` among them) still works, and so does
        // one that knows `level` and `exp` (v5) and reads up to them.
        assert!(l.contains(" rtt_ms=-1 party=3 "), "{l}");
        assert!(l.contains(" level=7 exp=626 items="), "{l}");
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

            let line = smoke_line(&Smoke { name, area: &area, fixture: true, peers_seen: 1, updates: 2, rejected: 3, rtt: None, party: 1, battles: 0, victories: 0, area_changes: 0, level: 0, exp: 0, items: 0 });
            assert_eq!(line.lines().count(), 1, "{line:?}");
            assert_eq!(line.split_whitespace().count(), 15, "SMOKE + 14 fields: {line:?}");
            for field in line.split_whitespace().skip(1) {
                assert!(field.split_once('=').is_some(), "{field:?} in {line:?}");
            }
            let first = line.split_whitespace().nth(1).unwrap();
            assert_eq!(first, format!("name={enc}"));
        }
    }
}

#[cfg(test)]
mod interact_tests;
#[cfg(test)]
mod scene_tests;
