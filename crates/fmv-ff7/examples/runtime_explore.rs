//! Play the story headlessly on the live field runtime, field after field, and say how far it gets (needs the owner's
//! data). The route solver behind `docs/disc1-live-route.txt`.
//!
//! ```text
//! FMV_FF7_DATA=<.../data> cargo run --release -p fmv-ff7 --example runtime_explore -- \
//!     [--from FIELD] [--until FIELD] [--tri N] [--moment M] [--set SEL:ADDR=VALUE]... [--steps N] [--route-out FILE] \
//!     [--choice FIELD:DIALOG=INDEX]...
//! ```
//!
//! A **state** is a running field and where the player stands in it. The search keeps a queue of states, always
//! expanding the one with the furthest story moment (then the best last action, then the most recent). Expanding a
//! state lets the field settle and then tries every action a player could take from where it stands:
//!
//! * stand on a walkmesh triangle, with and without OK held;
//! * talk to a model (stand beside it, face it, press OK);
//! * walk across a line either way, or stand on it holding OK;
//! * walk through one of the field's gateways.
//!
//! Only what the player can **reach** counts: triangles joined to its own through the walkmesh's links without
//! entering a triangle a script has locked (`IDLCK`), and gateways only while the scripts allow map jumps (`MPJPO`).
//! An action is worth keeping when it moves the story moment, leaves the field, or changes the story or the field's
//! own scratch (ignoring scratch bytes that merely follow where the player stands). Windows are answered with their
//! first choice unless a field/dialog override is supplied; battles count as won, movies, menus and tutorials
//! complete at once. Choice indexes are zero-based relative to the question's first choice. Exported `@choice`
//! headers carry the same immutable policy into replay, including choices made in automatic entry scenes.
//!
//! It prints one line per expansion and, at the end, the chain of actions that led to the furthest moment. Numbers
//! and names only, never game text. Candidate actions are replayed by walking at the route bot's default speed.
//! Voluntary stationary time is exported as `idle` commands; locked scenes and dialogue are left to the bot.
//! A player action after an internal idle is not exported: a trailing wait cannot preserve that
//! order. Network sampling, dialogue response time and the bot's entry delays still make a route a lead, not proof.
#[path = "../tests/script_common.rs"]
mod common;

use common::{data_dir, flevel_path, load_field};
use fmv_ff7::field::{FieldFile, Gateway, Walkmesh};
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::runtime::{direction_of, Banks, Command, Event, FieldRuntime, PlayerInput, PERSISTENT_BYTES};
use fmv_ff7::script::{decode_at, Maplist};
use fmv_ff7::story::StoryState;
use std::collections::{BTreeMap, BinaryHeap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

// Entry scenes (including the cargo car) can run longer than thirty seconds.
const SETTLE_FRAMES: u32 = 3600;
const PROBE_FRAMES: u32 = 300;
const FOLLOW_FRAMES: u32 = 3600;
const FIELD_FPS: f64 = 30.0;
/// Match scripts/live-route.py's default --speed, rather than walking 24 units per field frame (720 per second).
const WALK_UNITS_PER_SECOND: f64 = 200.0;
/// Expansions allowed per (field, moment): a field whose actions only shuffle its scratch must not eat the search.
const PER_PLACE: u32 = 10;
/// The pad's OK bit.
const OK: u16 = 0x20;

type Pos = (i32, i32, i32);

#[derive(Debug, Default)]
struct ChoicePolicy(BTreeMap<(String, u16), u8>);

impl ChoicePolicy {
    fn insert(&mut self, directive: &str) -> Result<(), String> {
        let invalid = || format!("invalid --choice {directive:?}; expected FIELD:DIALOG=INDEX (ASCII field, dialog 0..65535, index 0..255)");
        let (field, answer) = directive.split_once(':').ok_or_else(invalid)?;
        let (dialog, index) = answer.split_once('=').ok_or_else(invalid)?;
        if field.is_empty() || !field.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            || dialog.is_empty() || !dialog.bytes().all(|b| b.is_ascii_digit())
            || index.is_empty() || !index.bytes().all(|b| b.is_ascii_digit()) {
            return Err(invalid());
        }
        let dialog = dialog.parse::<u16>().map_err(|_| invalid())?;
        let index = index.parse::<u8>().map_err(|_| invalid())?;
        let key = (field.to_string(), dialog);
        if self.0.contains_key(&key) {
            return Err(format!("duplicate --choice for {field}:{dialog}"));
        }
        self.0.insert(key, index);
        Ok(())
    }

    fn headers(&self) -> Vec<String> {
        self.0.iter().map(|((field, dialog), index)| format!("@choice {field} {dialog} {index}")).collect()
    }
}

/// Shared run policy plus the field currently being played; probes borrow the same policy as honest playback.
struct Playback<'a> {
    field: &'a str,
    choices: &'a ChoicePolicy,
    /// One question answered otherwise for a retried probe: (dialog, choice).
    trial: Option<(u16, u8)>,
}

impl Playback<'_> {
    fn answer(&self, dialog: u8, (first, last): (u8, u8)) -> Result<u8, String> {
        let choice = match self.trial {
            Some((d, c)) if d == u16::from(dialog) => c,
            _ => self.choices.0.get(&(self.field.to_string(), u16::from(dialog))).copied().unwrap_or(0),
        };
        if first > last || choice > last - first {
            return Err(format!("choice {}:{dialog}={choice} is out of range for ASK lines {first}..{last}; indexes are zero-based", self.field));
        }
        Ok(choice)
    }
}

fn fatal(message: String) -> ! {
    eprintln!("runtime_explore: {message}");
    std::process::exit(2)
}

fn moment_of(banks: &Banks) -> u16 {
    u16::from_le_bytes([banks.get(0), banks.get(1)])
}

/// A fingerprint of a story (persistent bytes the engine does not own).
fn story_key(banks: &Banks) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for i in 0..PERSISTENT_BYTES {
        let sel = [1u8, 3, 11, 13, 15][i / 256];
        let b = banks.get(i) & !fmv_ff7::story::engine_owned_bits(sel, (i % 256) as u8);
        h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
    }
    h
}

#[derive(Debug, Clone, Default)]
struct Outcome {
    jump: Option<(u16, i16, i16, i16)>,
    battles: Vec<u16>,
    movies: Vec<u16>,
    windows: u32,
    game_over: bool,
    idle: IdleTime,
    control_taken: bool,
    /// Dialogs of windows open now (by window), and of the windows that finished closing, in order.
    open_dialogs: BTreeMap<u8, u8>,
    closed: Vec<u8>,
    /// The questions met: (dialog, how many choices).
    asks: Vec<(u8, u8)>,
}

/// A compact suffix of voluntary waiting. The bot can keep this suffix through a forced scene, but cannot replay
/// it before a later player action without a richer trace. Do not silently move such a pause to the end instead.
#[derive(Debug, Clone, Default)]
struct IdleTime {
    frames: u32,
    acted: bool,
    action_after_idle: bool,
}

impl IdleTime {
    fn acted(&mut self) {
        self.acted = true;
        self.action_after_idle |= self.frames > 0;
    }

    fn absorb(&mut self, more: Self) {
        self.action_after_idle |= more.action_after_idle || (self.frames > 0 && more.acted);
        self.frames += more.frames;
        self.acted |= more.acted;
    }

    /// Append one pause, not one route command per frame. None means this timing cannot be a suffix.
    fn commands(&self, mut commands: String) -> Option<String> {
        if self.action_after_idle {
            return None;
        }
        if self.frames > 0 {
            if !commands.is_empty() {
                commands.push_str("; ");
            }
            commands.push_str(&format!("idle {:.3}", f64::from(self.frames) / FIELD_FPS));
        }
        Some(commands)
    }
}

impl Outcome {
    fn absorb(&mut self, more: Outcome) {
        self.jump = more.jump;
        self.battles.extend(more.battles);
        self.movies.extend(more.movies);
        self.windows += more.windows;
        self.game_over |= more.game_over;
        self.idle.absorb(more.idle);
        self.control_taken |= more.control_taken;
    }
}

/// Answer what the field asked since the last frame. Returns true when the field is left (a map jump) or over.
fn pump(ctx: &Playback<'_>, rt: &mut FieldRuntime, out: &mut Outcome) -> bool {
    // The bot discards unfinished movement when the host takes control, but not for ordinary dialogue alone.
    out.control_taken |= rt.player_scripted();
    if rt.pending_tutorial().is_some() {
        rt.complete_tutorial();
    }
    for ev in rt.drain_events() {
        match ev {
            Event::WindowDone { window } | Event::WindowClosedByScript { window } => {
                if let Some(dialog) = out.open_dialogs.remove(&window) {
                    out.closed.push(dialog);
                }
            }
            Event::Window { window, dialog, ask, .. } => {
                out.windows += 1;
                out.open_dialogs.insert(window, dialog);
                if let Some((first, last)) = ask {
                    out.asks.push((dialog, last.saturating_sub(first) + 1));
                }
                if let Some(lines) = ask {
                    let choice = ctx.answer(dialog, lines).unwrap_or_else(|error| fatal(error));
                    if !rt.answer(window, choice) {
                        fatal(format!("runtime refused choice {}:{dialog}={choice} in window {window}", ctx.field));
                    }
                } else {
                    rt.close_window(window);
                }
            }
            Event::Command(Command::MapJump { field, x, y, triangle, .. }) | Event::Command(Command::Minigame { field, x, y, triangle, .. }) => {
                out.jump = Some((field, x, y, triangle));
                return true;
            }
            Event::Command(Command::Battle { formation }) => {
                out.battles.push(formation);
                rt.set_battle_result(0);
                rt.complete_command();
            }
            Event::Command(Command::Movie { movie }) => {
                out.movies.push(movie);
                rt.complete_command();
            }
            Event::Command(Command::GameOver) => {
                out.game_over = true;
                return true;
            }
            Event::Command(_) => rt.complete_command(),
            _ => {}
        }
    }
    false
}

/// A locked scene that loops until OK is pressed at the right moment (smkin_1's three-person door after reactor 5's
/// bomb: Barret and Tifa press their buttons a fixed time into each round and Cloud's press must land with theirs).
/// Tries OK `k` frames after the next window finishes closing, for every `k` up to three seconds, and returns the
/// first press after which the story moved: (dialog of that window, k, the field after the press, what happened).
/// The bot replays it as `okafter DIALOG SECONDS`: OK that long after it sees a window with that dialog close.
fn timed_press(ctx: &Playback<'_>, rt: &FieldRuntime, at: Pos) -> Option<(u8, u32, FieldRuntime, Outcome, Pos)> {
    // The middle of the first run of delays that work: a networked press lands a frame or two either way.
    let mut works = Vec::new();
    for k in 0..=90u32 {
        match press_after(ctx, rt, at, k) {
            Some(found) => works.push(found),
            None if works.is_empty() => continue,
            None => break,
        }
    }
    let middle = works.len() / 2;
    works.into_iter().nth(middle)
}

/// One `timed_press` probe: OK `k` frames after the next window finishes closing.
fn press_after(ctx: &Playback<'_>, rt: &FieldRuntime, at: Pos, k: u32) -> Option<(u8, u32, FieldRuntime, Outcome, Pos)> {
    let before = story_key(&rt.mem.banks);
    {
        let mut probe = rt.clone();
        let mut out = Outcome::default();
        let mut at = at;
        let mut mark: Option<(u8, u32)> = None;
        for f in 0..2400u32 {
            let press_at = mark.map(|(_, c)| c + k);
            let press = press_at == Some(f);
            let held = press_at.is_some_and(|p| f >= p && f < p + 5);
            if probe.player_scripted() {
                if let Some(m) = probe.player_model() {
                    at = (m.x, m.y, m.z);
                }
            }
            probe.tick(Some(PlayerInput { x: at.0, y: at.1, z: at.2, ok: press, keys_held: if held { OK } else { 0 }, keys_pressed: if press { OK } else { 0 }, ..Default::default() }));
            let closed = out.closed.len();
            let left = pump(ctx, &mut probe, &mut out);
            if mark.is_none() && out.closed.len() > closed {
                mark = Some((out.closed[closed], f));
            }
            // After the press, the scene runs on until it gives control back (or twenty seconds pass).
            if left || press_at.is_some_and(|p| f > p + 600 || (f > p + 30 && !busy(&probe))) {
                break;
            }
        }
        if std::env::var_os("FMV_EXPLORE_PRESS").is_some() {
            println!("      press k={k}: mark {mark:?} frame {} scratch 11..14 {:?} windows {} closed {:?}", probe.frame(), &probe.mem.scratch[11..14], out.windows, out.closed);
        }
        let (dialog, _) = mark?;
        if !out.game_over && (out.jump.is_some() || story_key(&probe.mem.banks) != before) {
            return Some((dialog, k, probe, out, at));
        }
    }
    None
}

/// A locked scene that waits for a direction held on the pad (`IFKEY`: mds5_4's stepping stones hop Cloud along while
/// left is held, then give control back). Holds each direction until control returns (or 40 s) and keeps the first that
/// moved the story or carried the player more than 100 units: (key name, the field after, what happened, where).
fn held_key(ctx: &Playback<'_>, rt: &FieldRuntime, at: Pos) -> Option<(&'static str, FieldRuntime, Outcome, Pos)> {
    let before = story_key(&rt.mem.banks);
    for (name, bit) in [("left", 0x8000u16), ("right", 0x2000), ("up", 0x1000), ("down", 0x4000)] {
        let mut probe = rt.clone();
        let mut out = Outcome::default();
        let mut here = at;
        let mut left = false;
        for f in 0..1200u32 {
            if probe.player_scripted() {
                if let Some(m) = probe.player_model() {
                    here = (m.x, m.y, m.z);
                }
            }
            probe.tick(Some(PlayerInput { x: here.0, y: here.1, z: here.2, keys_held: bit, keys_pressed: if f == 0 { bit } else { 0 }, ..Default::default() }));
            if pump(ctx, &mut probe, &mut out) {
                left = true;
                break;
            }
            if f > 10 && !busy(&probe) {
                break;
            }
        }
        if let Some(m) = probe.player_model() {
            here = (m.x, m.y, m.z);
        }
        let carried = (f64::from(here.0 - at.0)).hypot(f64::from(here.1 - at.1)) > 100.0;
        if !out.game_over && (left && out.jump.is_some() || (!busy(&probe) && (carried || story_key(&probe.mem.banks) != before))) {
            return Some((name, probe, out, here));
        }
    }
    None
}

/// Whether the field holds the player or waits for it.
fn busy(rt: &FieldRuntime) -> bool {
    rt.player_scripted() || rt.window_waiting() || rt.command().is_some()
}

fn report_busy(rt: &FieldRuntime, section: &[u8]) {
    println!("    busy frame={} lock={} window_waiting={} command={:?} scratch5:7={} scratch5:8={}", rt.frame(), rt.state.character_lock, rt.window_waiting(), rt.command(), rt.mem.scratch[7], rt.mem.scratch[8]);
    for entity in 0..rt.entity_count() {
        let Some((priority, pc)) = rt.entity_state(entity) else { continue };
        if rt.entity_broken(entity) { continue; }
        let instruction = decode_at(section, pc, 0);
        let waiting = instruction.as_ref().is_ok_and(|op| op.mnemonic.starts_with("REQ") || op.mnemonic == "WAIT");
        if priority < 7 || waiting {
            let instruction = instruction.map(|op| op.to_string()).unwrap_or_else(|error| format!("PC {pc:#06x}: {error}"));
            println!("    entity {entity} {} priority={priority} {instruction}", rt.entity_name(entity).unwrap_or("?"));
        }
    }
    for (window, open) in rt.windows().iter().enumerate().filter_map(|(i, w)| w.open.as_ref().map(|open| (i, open))) {
        println!("    window {window} entity={} dialog={} closed={} ask={:?}", open.entity, open.dialog, open.closed, open.ask);
    }
    println!("    PCs show current instructions; queued requests and remaining WAIT frames are not exposed");
}

/// Walk `approach`, a point a frame, the way a player walks there. Returns whether the player arrived (`false`: a
/// script took over on the way and changed something: the story, the field's scratch or where the player stands) and
/// where it is. A script that only says something on the way (bumping into someone) is heard out and the walk goes on.
fn walk_to(ctx: &Playback<'_>, rt: &mut FieldRuntime, approach: &[Pos], from: Pos, out: &mut Outcome) -> (bool, Pos) {
    let mut at = from;
    let mut chats = 0;
    for p in approach {
        // Someone solid in the way stops the player (the host refuses the step). Whoever walked into the way gets a
        // moment to walk on; someone who stays is bumped into (their contact script runs) and the walk ends there.
        let p = *p;
        if rt.player_step_blocked(at, p) {
            for _ in 0..45 {
                let free = !busy(rt);
                rt.tick(Some(PlayerInput { x: at.0, y: at.1, z: at.2, ..Default::default() }));
                if free && !busy(rt) {
                    out.idle.frames += 1;
                }
                if pump(ctx, rt, out) {
                    return (false, at);
                }
                if busy(rt) || !rt.player_step_blocked(at, p) {
                    break;
                }
            }
            if busy(rt) || rt.player_step_blocked(at, p) {
                if std::env::var_os("FMV_EXPLORE_JUMPS").is_some() {
                    for m in rt.step_contacts(at, p) {
                        let who = &rt.models()[m];
                        println!("        stopped at {},{},{} stepping to {},{} by model {m} ({}) at {},{},{} solid range {} (player's {})", at.0, at.1, at.2, p.0, p.1, rt.entity_name(who.entity).unwrap_or(""), who.x, who.y, who.z, who.solid_range, rt.player_solid_range());
                    }
                }
                rt.player_bumped(at, p);
                let (more, end) = run_idle(ctx, rt, FOLLOW_FRAMES, 0, &[], at, 0, false);
                out.absorb(more);
                return (false, end);
            }
        }
        if p != at {
            out.idle.acted();
            out.control_taken = false;
        }
        at = p;
        rt.tick(Some(PlayerInput { x: at.0, y: at.1, z: at.2, ..Default::default() }));
        let before = out.windows;
        if pump(ctx, rt, out) {
            return (false, at);
        }
        if busy(rt) || out.windows != before {
            let (story, scratch) = (story_key(&rt.mem.banks), rt.mem.scratch);
            let (more, end) = run_idle(ctx, rt, FOLLOW_FRAMES, 0, &[], at, 0, false);
            let over = more.jump.is_some() || more.game_over;
            out.absorb(more);
            let moved = (end.0 - at.0).abs() + (end.1 - at.1).abs() > 8;
            chats += 1;
            if over || moved || chats > 6 || story != story_key(&rt.mem.banks) || scratch != rt.mem.scratch {
                return (false, end);
            }
        }
    }
    (true, at)
}

/// Run up to `frames` frames. The player walks `path` (a point a frame), then stands at `at`, facing `dir`, holding OK
/// for five frames from the second when `ok`. While a script holds the player, the player follows where the scripts
/// put it. Stops early once at least `min` frames ran and the field has been idle (no script holds the player, no
/// window is open, no command is pending) for twenty frames: the story's clocks should not run while nothing happens.
/// Returns what happened and where the player ends.
fn run_idle(ctx: &Playback<'_>, rt: &mut FieldRuntime, frames: u32, min: u32, path: &[Pos], mut at: Pos, dir: u8, ok: bool) -> (Outcome, Pos) {
    let mut out = Outcome::default();
    let (mut idle, mut settling) = (0u32, 0u32);
    for f in 0..frames {
        // A script still running without holding the player (someone walking to a door to open it) is waited for, but
        // not for ever: some run as long as the field does.
        if busy(rt) {
            idle = 0;
            settling = 0;
        } else if rt.scripts_running() && settling < 300 {
            idle = 0;
            settling += 1;
        } else {
            idle += 1;
        }
        if f >= min.max(path.len() as u32 + 8) && idle >= 20 {
            break;
        }
        let free = !busy(rt);
        let walking = path.get(f as usize).is_some_and(|p| *p != at);
        if let Some(p) = path.get(f as usize) {
            if free && walking {
                out.idle.acted();
                out.control_taken = false;
            }
            at = *p;
        }
        if rt.player_scripted() {
            if let Some(m) = rt.player_model() {
                if std::env::var_os("FMV_EXPLORE_JUMPS").is_some() && (m.x - at.0).abs() + (m.y - at.1).abs() + (m.z - at.2).abs() > 30 {
                    println!("        frame {f}: the scripts put the player (model {:?}) at {},{},{} from {},{},{}", rt.state.pc_model, m.x, m.y, m.z, at.0, at.1, at.2);
                }
                at = (m.x, m.y, m.z);
            }
        }
        let held = ok && (2..=6).contains(&f);
        if free && ok && f == 2 {
            out.idle.acted();
        }
        let player = PlayerInput { x: at.0, y: at.1, z: at.2, dir, ok: ok && f == 2, keys_held: if held { OK } else { 0 }, keys_pressed: if ok && f == 2 { OK } else { 0 }, ..Default::default() };
        rt.tick(Some(player));
        // Check both sides of the frame before pump dismisses a new window. The OK press/hold is an action, not
        // voluntary idle, and a scripted walk is already represented by the bot waiting for control to return.
        if free && !busy(rt) && path.get(f as usize).is_none() && !(ok && f <= 6) {
            out.idle.frames += 1;
        }
        if pump(ctx, rt, &mut out) {
            return (out, at);
        }
    }
    if rt.player_scripted() {
        if let Some(m) = rt.player_model() {
            at = (m.x, m.y, m.z);
        }
    }
    (out, at)
}

fn centre(walk: &Walkmesh, t: usize) -> Pos {
    let v = walk.triangles[t];
    let s = |f: fn(&fmv_ff7::field::Vertex) -> i16| (i32::from(f(&v[0])) + i32::from(f(&v[1])) + i32::from(f(&v[2]))) / 3;
    (s(|p| p.x), s(|p| p.y), s(|p| p.z))
}

/// The triangle across edge `e` of triangle `t`, if a player may step into it: it exists and no script locked it.
/// Someone solid standing there does not close it: triangles are big, a player slides round a character, and whether
/// there is room to is found by walking ([`walk_to`]), not by looking at the triangle's centre.
fn link_open(walk: &Walkmesh, rt: &FieldRuntime, t: usize, e: usize) -> Option<usize> {
    let n = usize::from(walk.access[t][e]);
    (n < walk.triangles.len() && !rt.triangle_locked(n as u16)).then_some(n)
}

/// The triangles the player can walk to from `from` without entering a locked one.
fn reachable(walk: &Walkmesh, rt: &FieldRuntime, from: usize) -> Vec<bool> {
    let mut seen = vec![false; walk.triangles.len()];
    if from >= seen.len() {
        return seen;
    }
    seen[from] = true;
    let mut queue = VecDeque::from([from]);
    while let Some(t) = queue.pop_front() {
        for e in 0..3 {
            if let Some(n) = link_open(walk, rt, t, e).filter(|n| !seen[*n]) {
                seen[n] = true;
                queue.push_back(n);
            }
        }
    }
    seen
}

/// The model that stops the straight walk from `a` to `b`, if one does; `Err` when the walk leaves the walkmesh,
/// enters a locked triangle, or passes between two triangles that are not linked (a wall inside the mesh) instead.
fn stopped_by(walk: &Walkmesh, rt: &FieldRuntime, a: Pos, b: Pos) -> Result<Option<usize>, ()> {
    let (dx, dy, dz) = (f64::from(b.0 - a.0), f64::from(b.1 - a.1), f64::from(b.2 - a.2));
    let n = (dx.hypot(dy) / 6.0).ceil().max(1.0) as i32;
    let mut prev = a;
    let mut tri = rt.triangle_at(a.0, a.1, a.2).ok_or(())?;
    for k in 1..=n {
        let f = f64::from(k) / f64::from(n);
        let q = (a.0 + (dx * f) as i32, a.1 + (dy * f) as i32, a.2 + (dz * f) as i32);
        let t = rt.triangle_at(q.0, q.1, q.2).filter(|t| !rt.triangle_locked(*t)).ok_or(())?;
        if t != tri && !walk.access.get(usize::from(tri)).is_some_and(|links| links.contains(&t)) {
            return Err(());
        }
        tri = t;
        if let Some(m) = rt.step_contacts(prev, q).first() {
            return Ok(Some(*m));
        }
        prev = q;
    }
    Ok(None)
}

/// A corner to walk by on the way from `a` to `b` when someone solid stands on the straight line: beside them, on
/// whichever side leaves room to pass. `None` when nobody is in the way, or there is no room on either side.
fn detour(walk: &Walkmesh, rt: &FieldRuntime, a: Pos, b: Pos) -> Option<Pos> {
    let m = stopped_by(walk, rt, a, b).ok()??;
    let who = rt.models().get(m)?;
    let (dx, dy) = (f64::from(b.0 - a.0), f64::from(b.1 - a.1));
    let d = dx.hypot(dy).max(1.0);
    let (px, py) = (-dy / d, dx / d);
    let r = f64::from(rt.player_solid_range());
    let keep = (r + f64::from(who.solid_range)) / 2.0 + r * std::f64::consts::FRAC_1_SQRT_2;
    for margin in [8.0, 24.0, 48.0] {
        for side in [1.0, -1.0] {
            let by = ((f64::from(who.x) + px * side * (keep + margin)) as i32, (f64::from(who.y) + py * side * (keep + margin)) as i32, a.2);
            if stopped_by(walk, rt, a, by) == Ok(None) && stopped_by(walk, rt, by, b) == Ok(None) {
                return Some(by);
            }
        }
    }
    None
}

/// The corners a player actually walks by: `corners` (a triangle-by-triangle path from `from`) with every corner
/// left out that a straight walk over linked triangles makes unnecessary, and a corner added beside whoever stands
/// on the way where there is room to pass them. Where there is none the path goes on into them: the walk stops there.
fn smooth(walk: &Walkmesh, rt: &FieldRuntime, from: Pos, corners: &[Pos]) -> Vec<Pos> {
    const AHEAD: usize = 10;
    let mut path = Vec::new();
    let (mut cur, mut i) = (from, 0);
    // Standing within someone's reach (they walked up, or stepped aside onto the player): the way on is blocked on
    // every side but away from them. Step back out first.
    if let Some(first) = corners.first() {
        let (dx, dy) = (f64::from(first.0 - from.0), f64::from(first.1 - from.1));
        let d = dx.hypot(dy).max(1.0);
        let ahead = (from.0 + (dx / d * 6.0) as i32, from.1 + (dy / d * 6.0) as i32, from.2);
        if let Some(who) = rt.step_contacts(from, ahead).first().and_then(|m| rt.models().get(*m)) {
            let (ax, ay) = (f64::from(from.0 - who.x), f64::from(from.1 - who.y));
            let a = ay.atan2(ax);
            let out = [0.0f64, 0.5, -0.5, 1.0, -1.0].into_iter().flat_map(|turn| [56.0, 88.0, 120.0].map(|far| (a + turn, far))).find_map(|(angle, far)| {
                let to = (from.0 + (angle.cos() * far) as i32, from.1 + (angle.sin() * far) as i32, from.2);
                (stopped_by(walk, rt, from, to) == Ok(None)).then_some(to)
            });
            if let Some(out) = out {
                path.push(out);
                cur = out;
            }
        }
    }
    while i < corners.len() {
        let last = (i + AHEAD).min(corners.len() - 1);
        if let Some(j) = (i..=last).rev().find(|j| stopped_by(walk, rt, cur, corners[*j]) == Ok(None)) {
            path.push(corners[j]);
            (cur, i) = (corners[j], j + 1);
            continue;
        }
        // Nothing ahead is in plain sight: go round whoever hides the nearest corner that has a way round.
        match (i..=(i + 3).min(corners.len() - 1)).find_map(|j| detour(walk, rt, cur, corners[j]).map(|by| (j, by))) {
            Some((j, by)) => {
                path.push(by);
                path.push(corners[j]);
                (cur, i) = (corners[j], j + 1);
            }
            None => {
                path.push(corners[i]);
                (cur, i) = (corners[i], i + 1);
            }
        }
    }
    path
}

/// The way a player walks from `from` (standing on triangle `from_tri`) to `to` on triangle `to_tri`: through the
/// walkmesh's links, never into a locked triangle, at the bot's default walking speed (one point per field frame).
/// The corners of a way through the triangles `prev` leads back along from `to_tri`, kept clear of `lines` (see
/// [`way_with`]).
fn clear_corners(walk: &Walkmesh, prev: &[Option<(usize, usize)>], from_tri: usize, to_tri: usize, from: Pos, to: Pos, lines: &[((f64, f64), (f64, f64))]) -> Vec<Pos> {
    // The push reaches the player's solid range (34 at scale 512) from the line; keep a margin.
    const CLEAR: f64 = 40.0;
    let xy = |p: Pos| (f64::from(p.0), f64::from(p.1));
    let vert = |v: &fmv_ff7::field::Vertex| (i32::from(v.x), i32::from(v.y), i32::from(v.z));
    // The crossings, first to last: (the triangle entered, the shared edge's ends).
    let mut crossings = Vec::new();
    let mut t = to_tri;
    while let Some((p, e)) = prev[t] {
        crossings.push((p, t, vert(&walk.triangles[p][e]), vert(&walk.triangles[p][(e + 1) % 3])));
        t = p;
    }
    let _ = from_tri;
    crossings.reverse();
    let mut corners: Vec<Pos> = Vec::new();
    let mut cur = from;
    for (left, entered, a, b) in crossings {
        // The point of the edge (kept off its ends, which may be walls) farthest from every line.
        let best = (2..=18).map(|k| {
            let f = f64::from(k) / 20.0;
            ((f64::from(a.0) + f64::from(b.0 - a.0) * f) as i32, (f64::from(a.1) + f64::from(b.1 - a.1) * f) as i32, (f64::from(a.2) + f64::from(b.2 - a.2) * f) as i32)
        }).max_by(|p, q| {
            let dp = lines.iter().map(|l| seg_point_dist(xy(*p), *l)).fold(f64::INFINITY, f64::min);
            let dq = lines.iter().map(|l| seg_point_dist(xy(*q), *l)).fold(f64::INFINITY, f64::min);
            dp.total_cmp(&dq)
        }).unwrap_or(a);
        if seg_clearance(xy(cur), xy(best), lines) < CLEAR {
            // Round by a point inside the triangle just left or the one entered: of a grid of points across each, the
            // one whose two legs stay farthest from every line (ealin_2: between the creaking floor and Elmyra's door).
            let mut detour: Option<(f64, Pos)> = None;
            for tt in [left, entered] {
                let tri = &walk.triangles[tt];
                for i in 1..8 {
                    for j in 1..(8 - i) {
                        let k = 8 - i - j;
                        let at = |f: fn(&fmv_ff7::field::Vertex) -> i16| (i32::from(f(&tri[0])) * i + i32::from(f(&tri[1])) * j + i32::from(f(&tri[2])) * k) / 8;
                        let q = (at(|v| v.x), at(|v| v.y), at(|v| v.z));
                        let clear = seg_clearance(xy(cur), xy(q), lines).min(seg_clearance(xy(q), xy(best), lines));
                        if clear >= CLEAR && detour.is_none_or(|(c, _)| clear > c) {
                            detour = Some((clear, q));
                        }
                    }
                }
            }
            let detour = detour.map(|(_, q)| q);
            if let Some(q) = detour {
                corners.push(q);
            }
        }
        corners.push(best);
        cur = best;
    }
    corners.push(to);
    corners
}

fn way(walk: &Walkmesh, rt: &FieldRuntime, from: Pos, from_tri: usize, to: Pos, to_tri: usize) -> Option<(Vec<Pos>, String)> {
    way_with(walk, rt, from, from_tri, to, to_tri, None)
}

/// The active lines' segments (x, y ends): their push and across scripts fire within the player's reach of them. ealin_2's
/// creaking floor by Aerith's door sends Cloud back to bed; the stairs are reached by keeping to the far side.
fn active_lines(rt: &FieldRuntime) -> Vec<((f64, f64), (f64, f64))> {
    rt.lines().iter().filter(|l| l.active).map(|l| ((f64::from(l.a.0), f64::from(l.a.1)), (f64::from(l.b.0), f64::from(l.b.1)))).collect()
}

fn seg_point_dist(p: (f64, f64), (a, b): ((f64, f64), (f64, f64))) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = (dx * dx + dy * dy).max(1.0);
    let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0);
    (p.0 - a.0 - t * dx).hypot(p.1 - a.1 - t * dy)
}

/// The least distance from the walk p->q to any of `lines` (sampled every few units).
fn seg_clearance(p: (f64, f64), q: (f64, f64), lines: &[((f64, f64), (f64, f64))]) -> f64 {
    let n = ((q.0 - p.0).hypot(q.1 - p.1) / 4.0).ceil().max(1.0) as usize;
    (0..=n).map(|k| {
        let f = k as f64 / n as f64;
        let x = (p.0 + (q.0 - p.0) * f, p.1 + (q.1 - p.1) * f);
        lines.iter().map(|l| seg_point_dist(x, *l)).fold(f64::INFINITY, f64::min)
    }).fold(f64::INFINITY, f64::min)
}

/// `way`, or with `lines` a way that keeps clear of them: each shared edge is crossed where it is farthest from the
/// lines, and a leg that would still pass within reach of one goes round by a point inside the triangle it crosses.
fn way_with(walk: &Walkmesh, rt: &FieldRuntime, from: Pos, from_tri: usize, to: Pos, to_tri: usize, lines: Option<&[((f64, f64), (f64, f64))]>) -> Option<(Vec<Pos>, String)> {
    let n = walk.triangles.len();
    if from_tri >= n || to_tri >= n {
        return None;
    }
    let mut prev: Vec<Option<(usize, usize)>> = vec![None; n];
    let mut seen = vec![false; n];
    seen[from_tri] = true;
    let mut queue = VecDeque::from([from_tri]);
    while let Some(t) = queue.pop_front() {
        if t == to_tri {
            break;
        }
        for e in 0..3 {
            if let Some(next) = link_open(walk, rt, t, e).filter(|next| !seen[*next]) {
                seen[next] = true;
                prev[next] = Some((t, e));
                queue.push_back(next);
            }
        }
    }
    if !seen[to_tri] {
        return None;
    }
    let mut corners = vec![to];
    let mut t = to_tri;
    while let Some((p, e)) = prev[t] {
        let (a, b) = (walk.triangles[p][e], walk.triangles[p][(e + 1) % 3]);
        if t != to_tri {
            corners.push(centre(walk, t));
        }
        corners.push(((i32::from(a.x) + i32::from(b.x)) / 2, (i32::from(a.y) + i32::from(b.y)) / 2, (i32::from(a.z) + i32::from(b.z)) / 2));
        t = p;
    }
    corners.reverse();
    let corners = match lines {
        None => smooth(walk, rt, from, &corners),
        Some(lines) => clear_corners(walk, &prev, from_tri, to_tri, from, to, lines),
    };
    let mut out = Vec::new();
    let mut legs = Vec::new();
    let mut cur = from;
    for c in corners {
        out.extend(walk_segment(cur, c));
        if c != cur {
            legs.push(format!("to {} {}", c.0, c.1));
        }
        cur = c;
    }
    // (the same corners as bot steps: the bot walks exactly this way)
    Some((out, legs.join("; ")))
}

fn walk_segment(from: Pos, to: Pos) -> Vec<Pos> {
    let (dx, dy, dz) = (f64::from(to.0 - from.0), f64::from(to.1 - from.1), f64::from(to.2 - from.2));
    let steps = ((dx * dx + dy * dy).sqrt() * FIELD_FPS / WALK_UNITS_PER_SECOND).ceil() as i32;
    (1..=steps)
        .map(|k| {
            let f = f64::from(k) / f64::from(steps);
            (from.0 + (dx * f) as i32, from.1 + (dy * f) as i32, from.2 + (dz * f) as i32)
        })
        .collect()
}

/// One step of a route.
struct Step {
    text: String,
    /// The step as `scripts/live-route.py` commands (`field: cmd; cmd`), empty when the field moves on by itself.
    cmds: String,
    parent: Option<Rc<Step>>,
}

fn chain(step: &Option<Rc<Step>>) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = step.clone();
    while let Some(s) = cur {
        out.push(s.text.clone());
        cur = s.parent.clone();
    }
    out.reverse();
    out
}

fn commands(step: &Option<Rc<Step>>) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = step.clone();
    while let Some(s) = cur {
        if !s.cmds.is_empty() {
            out.push(s.cmds.clone());
        }
        cur = s.parent.clone();
    }
    out.reverse();
    out
}

struct State {
    field: String,
    /// The running field (`None`: enter `field` fresh with `banks`).
    rt: Option<FieldRuntime>,
    banks: Banks,
    pos: Option<Pos>,
    tri: Option<usize>,
    via: Option<Rc<Step>>,
    /// What the field carries besides the story (items, gil): taken from the field before.
    carry: Option<(fmv_ff7::runtime::PartySaveData, Option<u32>)>,
}

struct Queued {
    key: (u16, i32, u64),
    state: State,
}

impl PartialEq for Queued {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}
impl Eq for Queued {}
impl PartialOrd for Queued {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Queued {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.key.cmp(&other.key)
    }
}

struct Loaded {
    field: FieldFile,
    walk: Arc<Walkmesh>,
    animation_counts: Vec<Vec<u16>>,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut start_field, mut steps, mut start_moment, mut route_out) = ("md1stin".to_string(), 200u32, None::<u16>, None::<String>);
    let (mut until, mut reached_target) = (None::<String>, false);
    let mut sets: Vec<(u8, u8, u8)> = Vec::new();
    let mut start_tri: Option<usize> = None;
    let (mut save_best, mut banks_file) = (None::<String>, None::<String>);
    let mut choices = ChoicePolicy::default();
    // A directory that gets the story on each first entry of a field at a moment (`FIELD-MOMENT.hex`), for the trace tool.
    let mut save_entries: Option<String> = None;
    while let Some(a) = args.next() {
        let v = args.next().unwrap_or_default();
        match a.as_str() {
            "--from" => start_field = v,
            "--until" => until = Some(v),
            "--steps" => steps = v.parse().unwrap_or(steps),
            "--moment" => start_moment = v.parse().ok(),
            "--route-out" => route_out = Some(v),
            "--tri" => start_tri = v.parse().ok(),
            "--save-best" => save_best = Some(v),
            "--save-entries" => save_entries = Some(v),
            "--banks" => banks_file = Some(v),
            "--choice" => choices.insert(&v).unwrap_or_else(|error| fatal(error)),
            "--set" => {
                if let Some((sa, val)) = v.split_once('=') {
                    if let (Some((sel, addr)), Ok(val)) = (sa.split_once(':'), val.parse::<u8>()) {
                        if let (Ok(sel), Ok(addr)) = (sel.parse::<u8>(), addr.parse::<u8>()) {
                            sets.push((sel, addr, val));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    let Some(dir) = data_dir() else {
        eprintln!("FMV_FF7_DATA is not set; skipping");
        return;
    };
    let mut lgp = LgpArchive::open_path(&flevel_path(&dir)).expect("flevel");
    let mut chars = LgpArchive::open_path(dir.join("field/char.lgp")).ok();
    let maplist = Maplist::from_archive(&mut lgp).expect("maplist");
    let mut banks = Banks::from_story(&StoryState::fresh_reset()).expect("complete");
    if let Some(b) = banks_file.as_deref().and_then(|f| std::fs::read_to_string(f).ok()).and_then(|h| Banks::from_hex(h.trim())) {
        banks = b;
    }
    if let Some(m) = start_moment {
        let [lo, hi] = m.to_le_bytes();
        banks.set(0, lo);
        banks.set(1, hi);
    }
    for (sel, addr, val) in &sets {
        if let Some(base) = fmv_ff7::runtime::memory::base_of(*sel) {
            banks.set(base + usize::from(*addr), *val);
        }
    }

    let mut fields: BTreeMap<String, Option<Rc<Loaded>>> = BTreeMap::new();
    let mut load = |name: &str, lgp: &mut LgpArchive<std::fs::File>| -> Option<Rc<Loaded>> {
        fields
            .entry(name.to_string())
            .or_insert_with(|| {
                let field = load_field(lgp, name).ok()?;
                let walk = Arc::new(field.walkmesh().ok()?);
                let animation_counts = match (chars.as_mut(), field.model_loader()) {
                    (Some(chars), Ok(loader)) => fmv_ff7::runtime::AnimState::frame_counts_from(chars, &loader),
                    _ => Vec::new(),
                };
                Some(Rc::new(Loaded { field, walk, animation_counts }))
            })
            .clone()
    };

    let mut heap: BinaryHeap<Queued> = BinaryHeap::new();
    let mut seq = 0u64;
    let mut push = |heap: &mut BinaryHeap<Queued>, state: State, score: i32| {
        seq += 1;
        heap.push(Queued { key: (moment_of(&state.banks), score, seq), state });
    };
    let start_pos = start_tri.and_then(|t| load(&start_field, &mut lgp).filter(|l| t < l.walk.triangles.len()).map(|l| centre(&l.walk, t)));
    push(&mut heap, State { field: start_field, rt: None, banks, pos: start_pos, tri: start_tri, via: None, carry: None }, 9);

    // (field, story, scratch-without-noise) images already expanded or queued.
    let mut images: HashSet<(String, u64, u64)> = HashSet::new();
    // Field entries tried: (field, story, arrival triangle). The triangle tells apart a field re-entered by its own map jump
    // at another place (ealin_2: sneaking downstairs at night jumps from ealin_2 to ealin_2).
    let mut entered: HashSet<(String, u64, i32)> = HashSet::new();
    let mut visited: HashSet<String> = HashSet::new();
    let mut per_place: BTreeMap<(String, u16, Option<usize>, bool), u32> = BTreeMap::new();
    let mut discovered: Vec<(String, u16, u8)> = Vec::new();
    let mut best: (u16, Option<Rc<Step>>, String) = (0, None, String::new());
    let mut unloadable: Vec<String> = Vec::new();

    let mut step = 0u32;
    while step < steps {
        let Some(Queued { state, .. }) = heap.pop() else {
            println!("nothing left to try");
            break;
        };
        let State { field: field_name, rt, banks, pos, tri, mut via, carry } = state;
        // Answers found by retried probes are the run's policy from now on (and the route's @choice headers).
        for (field, dialog, choice) in discovered.drain(..) {
            choices.0.entry((field, dialog)).or_insert(choice);
        }
        let ctx = &Playback { field: &field_name, choices: &choices, trial: None };
        let Some(loaded) = load(&field_name, &mut lgp) else {
            if !unloadable.contains(&field_name) {
                println!("{step:>4} {field_name:<10} cannot be loaded (another game mode): {}", chain(&via).last().cloned().unwrap_or_default());
                unloadable.push(field_name.clone());
            }
            continue;
        };
        let walk = Arc::clone(&loaded.walk);
        let section = loaded.field.section(fmv_ff7::field::section::SCRIPT).expect("script");
        let fresh = rt.is_none();
        let mut rt = match rt {
            Some(rt) => rt,
            None => {
                if !entered.insert((field_name.clone(), story_key(&banks), tri.map_or(-1, |t| t as i32))) {
                    continue;
                }
                let Ok(mut rt) = FieldRuntime::new(section, banks.clone()) else { continue };
                if let Some(dir) = &save_entries {
                    let file = format!("{dir}/{field_name}-{}.hex", moment_of(&banks));
                    if !std::path::Path::new(&file).exists() {
                        let _ = std::fs::write(&file, format!("{}\n", banks.to_hex()));
                    }
                }
                rt.set_walkmesh(Arc::clone(&walk));
                rt.set_animation_frame_counts(&loaded.animation_counts);
                if let Some((save, gil)) = &carry {
                    rt.set_save_data(save.clone());
                    rt.gil = *gil;
                }
                rt.init();
                rt
            }
        };
        // The budget is per walkmesh part as well: reaching another part of a field (a ladder, a jump) is progress, and
        // loops in one part must not use up the expansions a later part needs (smkin_3's climb back from reactor 5's bomb).
        let part = tri.filter(|t| *t < walk.triangles.len()).and_then(|t| reachable(&walk, &rt, t).iter().position(|r| *r));
        // A fresh entry (a map jump in, even into the same field) has its own budget: states still busy with a scene in
        // the same place must not use up the expansions a new arrival needs (ealin_2 re-entered downstairs at night).
        let place = (field_name.clone(), moment_of(&rt.mem.banks), part, fresh);
        let count = per_place.entry(place).or_insert(0);
        if *count >= PER_PLACE {
            continue;
        }
        *count += 1;
        step += 1;
        visited.insert(field_name.clone());

        // Where the player stands: where the state says, else where the field's own scripts put its character.
        let mut at = pos.or_else(|| rt.player_model().map(|m| (m.x, m.y, m.z))).unwrap_or_else(|| centre(&walk, 0));
        // (an arrival's height is its triangle's)
        if fresh {
            if let Some(t) = tri.filter(|t| *t < walk.triangles.len()) {
                at.2 = centre(&walk, t).2;
            }
        }
        let before = moment_of(&rt.mem.banks);
        let settle = if fresh || busy(&rt) { run_idle(ctx, &mut rt, SETTLE_FRAMES, 45, &[], at, 0, false) } else { (Outcome::default(), at) };
        let moved = settle.1 != at;
        at = settle.1;
        let now = moment_of(&rt.mem.banks);
        if settle.0.jump.is_none() && !settle.0.game_over {
            // The field's forced entry scene is automatic. Only the free time we deliberately spent standing
            // afterwards belongs in the route, before the next action that can depend on its timers.
            let cmds = settle.0.idle.commands(String::new()).expect("settling has no player walk");
            if !cmds.is_empty() {
                via = Some(Rc::new(Step { text: format!("{field_name}: {cmds} after entry scene"), cmds: format!("{field_name}: {cmds}"), parent: via }));
            }
        }
        if now > best.0 && !settle.0.game_over {
            best = (now, via.clone(), field_name.clone());
            // The story on entering the furthest field (before it settled), for the trace tool.
            if let Some(path) = &save_best {
                let _ = std::fs::write(path, format!("{}\n", banks.to_hex()));
            }
        }
        let carry_out = Some((rt.save_data().clone(), rt.gil));
        let enter = |to: &str, jump: (u16, i16, i16, i16), banks: Banks, via: Option<Rc<Step>>| State {
            field: to.to_string(),
            rt: None,
            banks,
            pos: Some((i32::from(jump.1), i32::from(jump.2), 0)),
            tri: usize::try_from(jump.3).ok(),
            via,
            carry: carry_out.clone(),
        };
        if settle.0.game_over {
            println!("{step:>4} {field_name:<10} game over on entry (moment {before})");
            continue;
        }
        if let Some(jump) = settle.0.jump {
            let to = maplist.name(jump.0).unwrap_or("?").to_string();
            println!("{step:>4} {field_name:<10} moment {before}->{now} on its own: battles {:?} movies {:?} windows {} -> {to}", settle.0.battles, settle.0.movies, settle.0.windows);
            let via = Some(Rc::new(Step { text: format!("{field_name}: (plays by itself, moment {before}->{now}) -> {to}"), cmds: String::new(), parent: via }));
            push(&mut heap, enter(&to, jump, rt.mem.banks.clone(), via), 5);
            continue;
        }
        if busy(&rt) {
            if let Some((dialog, k, probe, out, end)) = timed_press(ctx, &rt, at) {
                let after = moment_of(&probe.mem.banks);
                let secs = f64::from(k) / FIELD_FPS;
                println!("{step:>4} {field_name:<10} locked scene: OK {k} frames after dialog {dialog} closes moves the story (moment {now}->{after})");
                let text = format!("{field_name}: OK {k} frames after dialog {dialog} closes (moment {now}->{after}, battles {:?}, movies {:?}, windows {})", out.battles, out.movies, out.windows);
                let cmds = format!("{field_name}: okafter {dialog} {secs:.3}");
                let step_via = Some(Rc::new(Step { text, cmds, parent: via.clone() }));
                match out.jump {
                    Some(jump) => {
                        let to = maplist.name(jump.0).unwrap_or("?").to_string();
                        push(&mut heap, enter(&to, jump, probe.mem.banks.clone(), step_via), 5);
                    }
                    None => {
                        let banks = probe.mem.banks.clone();
                        let tri = probe.triangle_at(end.0, end.1, end.2).map(usize::from);
                        push(&mut heap, State { field: field_name.clone(), rt: Some(probe), banks, pos: Some(end), tri, via: step_via, carry: None }, 5);
                    }
                }
                continue;
            }
            if let Some((key, probe, out, end)) = held_key(ctx, &rt, at) {
                let after = moment_of(&probe.mem.banks);
                println!("{step:>4} {field_name:<10} locked scene: holding {key} gives control back (moment {now}->{after})");
                let text = format!("{field_name}: hold {key} (moment {now}->{after}, battles {:?}, movies {:?}, windows {})", out.battles, out.movies, out.windows);
                let step_via = Some(Rc::new(Step { text, cmds: format!("{field_name}: hold {key}"), parent: via.clone() }));
                match out.jump {
                    Some(jump) => {
                        let to = maplist.name(jump.0).unwrap_or("?").to_string();
                        push(&mut heap, enter(&to, jump, probe.mem.banks.clone(), step_via), 5);
                    }
                    None => {
                        let banks = probe.mem.banks.clone();
                        let tri = probe.triangle_at(end.0, end.1, end.2).map(usize::from);
                        push(&mut heap, State { field: field_name.clone(), rt: Some(probe), banks, pos: Some(end), tri, via: step_via, carry: None }, 4);
                    }
                }
                continue;
            }
            // A still-running scene is not a walking action. Otherwise its eventual progress is attributed to an
            // arbitrary candidate walk, and the exported route tells the TCP bot to walk somewhere it never went.
            println!("{step:>4} {field_name:<10} still busy after settling (moment {before}->{now}); no player actions tried");
            report_busy(&rt, section);
            continue;
        }
        let here = tri.filter(|_| fresh && !moved).or_else(|| rt.triangle_at(at.0, at.1, at.2).map(usize::from));
        let Some(here) = here.filter(|t| *t < walk.triangles.len()) else {
            println!("{step:>4} {field_name:<10} the player stands on no triangle at {at:?} (moment {now})");
            continue;
        };
        if until.as_deref() == Some(field_name.as_str()) {
            // Several real field transitions share one story moment. A focused replay needs the route to its
            // requested destination even when no higher moment displaced the earlier best state.
            best = (now, via.clone(), field_name.clone());
            reached_target = true;
            println!("target reached: {field_name} at moment {now}, triangle {here}");
            break;
        }
        let reach = reachable(&walk, &rt, here);
        let reach_count = reach.iter().filter(|r| **r).count();

        // What the field does by itself over a probe's time, and which scratch bytes only follow the player's position.
        let mut baseline = rt.clone();
        let _ = run_idle(ctx, &mut baseline, PROBE_FRAMES, 60, &[], at, 0, false);
        let mut noise = [false; 256];
        for t in (0..walk.triangles.len()).filter(|t| reach[*t]).step_by((reach_count / 4).max(1)).take(5) {
            let mut other = rt.clone();
            let _ = run_idle(ctx, &mut other, PROBE_FRAMES, 60, &[], centre(&walk, t), 0, false);
            for (i, n) in noise.iter_mut().enumerate() {
                *n |= other.mem.scratch[i] != baseline.mem.scratch[i];
            }
        }
        let scratch_key = |rt: &FieldRuntime| {
            let mut h: u64 = 0xcbf2_9ce4_8422_2325;
            for (i, b) in rt.mem.scratch.iter().enumerate() {
                if !noise[i] {
                    h = (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3);
                }
            }
            h
        };
        // Where a player is, as far as walking goes: the reachable set it stands in (its lowest triangle and its size).
        let place_of = |rt: &FieldRuntime, at: Pos| -> (usize, usize) {
            match rt.triangle_at(at.0, at.1, at.2).map(usize::from) {
                Some(t) => {
                    let r = reachable(&walk, rt, t);
                    (r.iter().position(|x| *x).unwrap_or(t), r.iter().filter(|x| **x).count())
                }
                None => (usize::MAX, 0),
            }
        };
        let here_place = (reach.iter().position(|x| *x).unwrap_or(here), reach_count);
        let image_key = |rt: &FieldRuntime, place: (usize, usize)| scratch_key(rt) ^ (place.0 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (place.1 as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
        images.insert((field_name.clone(), story_key(&rt.mem.banks), image_key(&baseline, here_place)));

        // The actions: (label, path, stand, dir, ok).
        let mut actions: Vec<(String, Vec<Pos>, Pos, u8, bool, String)> = Vec::new();
        for t in (0..walk.triangles.len()).filter(|t| reach[*t]) {
            let c = centre(&walk, t);
            actions.push((format!("tri {t}"), Vec::new(), c, 0, false, String::new()));
            // The probe presses OK facing direction 0: the bot must face that way too (mds6_2's slide answered the
            // solver's OK and not the bot's, which faced the way it had walked).
            actions.push((format!("tri {t}; ok"), Vec::new(), c, 0, true, "face 0; ok".to_string()));
        }
        let on_reach = |rt: &FieldRuntime, p: Pos| rt.triangle_at(p.0, p.1, p.2).is_some_and(|t| reach.get(usize::from(t)).copied().unwrap_or(false));
        let pc = rt.state.pc_model;
        if std::env::var_os("FMV_EXPLORE_TALK").is_some() {
            for (mi, m) in rt.models().iter().enumerate() {
                println!("      model {mi} {} at {},{} talk_off {} range {} solid {} visible {}", rt.entity_name(m.entity).unwrap_or(""), m.x, m.y, m.talk_off, m.talk_range, m.solid_range, m.visible);
            }
        }
        for (mi, m) in rt.models().iter().enumerate().filter(|(i, m)| Some(*i) != pc && !m.talk_off) {
            // As near as a player gets walking straight at them (their solid range stops it a stride short), and
            // within their talk range.
            let solid = f64::from(rt.player_solid_range());
            let near = (solid + f64::from(m.solid_range)) / 2.0 + solid + 6.0;
            let far = f64::from(m.talk_range) + solid - 4.0;
            if far < 12.0 {
                continue;
            }
            // Close up, and at the edge of the talk range: someone behind a counter or a locked floor (mrkt3's fatman1,
            // whose triangles kobun1 locks) is talked to from farther off.
            let mut rings = vec![near.min(far)];
            if far - 4.0 > near.min(far) + 8.0 {
                rings.push(far - 4.0);
            }
            // The two standing places nearest the player: whoever it is may be standing in the only way to the others.
            let mut stands: Vec<(i64, Pos, u8)> = rings
                .into_iter()
                .flat_map(|off| {
                    let diag = off * std::f64::consts::FRAC_1_SQRT_2;
                    [(-off, 0.0), (off, 0.0), (0.0, -off), (0.0, off), (-diag, -diag), (diag, diag), (-diag, diag), (diag, -diag)]
                })
                .map(|(ox, oy)| (ox as i32, oy as i32))
                .map(|(ox, oy)| ((m.x + ox, m.y + oy, m.z), direction_of(i64::from(-ox), i64::from(-oy))))
                .filter(|(stand, _)| on_reach(&rt, *stand))
                .map(|(stand, face)| (i64::from(stand.0 - at.0).pow(2) + i64::from(stand.1 - at.1).pow(2), stand, face))
                .collect();
            stands.sort();
            if std::env::var_os("FMV_EXPLORE_TALK").is_some() {
                println!("      talk candidates model {mi} {}: {} stands (near {near:.0}, far {far:.0})", rt.entity_name(m.entity).unwrap_or(""), stands.len());
            }
            for (_, stand, face) in stands.into_iter().take(2) {
                actions.push((format!("talk {} (model {mi}) from {},{}", rt.entity_name(m.entity).unwrap_or(""), stand.0, stand.1), Vec::new(), stand, face, true, format!("face {face}; ok")));
            }
        }
        for (li, l) in rt.lines().iter().enumerate().filter(|(_, l)| l.active) {
            let (dx, dy) = (f64::from(l.b.0 - l.a.0), f64::from(l.b.1 - l.a.1));
            let n = (dx * dx + dy * dy).sqrt().max(1.0);
            let (nx, ny) = (-dy / n, dx / n);
            // A point of the line (its middle first: a long line is often closed off there by someone standing on it,
            // and open further along), and the point `k` steps of five units off it there.
            let along = |t: f64| ((f64::from(l.a.0) + dx * t) as i32, (f64::from(l.a.1) + dy * t) as i32, (f64::from(l.a.2) + f64::from(l.b.2 - l.a.2) * t) as i32);
            let side = |t: f64, k: i32| {
                let m = along(t);
                (m.0 + (nx * 5.0 * f64::from(k)) as i32, m.1 + (ny * 5.0 * f64::from(k)) as i32, m.2)
            };
            const ALONG: [f64; 5] = [0.5, 0.25, 0.75, 0.1, 0.9];
            let name = rt.entity_name(l.entity).unwrap_or("").to_string();
            // A crossing is a walk over linked ground from thirty units before the line to as far beyond it as there is
            // ground (thirty units, or less at a ledge: a jump's line lies a stride from the edge). A probe put beyond
            // the ground would stand on whatever lies under it.
            let crossing = |sign: i32| {
                ALONG.into_iter().find_map(|t| {
                    let from = side(t, -6 * sign);
                    if !on_reach(&rt, from) {
                        return None;
                    }
                    [6, 3, 2, 1].into_iter().find(|far| stopped_by(&walk, &rt, from, side(t, far * sign)).is_ok()).map(|far| (t, far))
                })
            };
            for sign in [1, -1] {
                if let Some((t, far)) = crossing(sign) {
                    let (from, to) = (side(t, -6 * sign), side(t, far * sign));
                    let path: Vec<Pos> = std::iter::once(from).chain(walk_segment(from, to)).collect();
                    actions.push((format!("line {name} ({li}) across from {},{}", from.0, from.1), path, to, 0, false, format!("to {} {}", to.0, to.1)));
                }
            }
            // OK at the line: stand a few units off it on a reachable side, facing it (the original wants the line
            // in front of the player), or on the line itself when neither side can be stood on.
            let mut stood = false;
            for t in ALONG {
                for k in [-2, 2, -4, 4] {
                    let stand = side(t, k);
                    if on_reach(&rt, stand) {
                        let face = direction_of((-nx * f64::from(k) * 100.0) as i64, (-ny * f64::from(k) * 100.0) as i64);
                        actions.push((format!("line {name} ({li}); ok from {},{} facing {face}", stand.0, stand.1), Vec::new(), stand, face, true, format!("face {face}; ok")));
                        stood = true;
                    }
                }
                if stood {
                    break;
                }
            }
            if !stood {
                if let Some(on) = ALONG.into_iter().map(along).find(|p| on_reach(&rt, *p)) {
                    actions.push((format!("line {name} ({li}); ok at {},{}", on.0, on.1), Vec::new(), on, 0, true, "ok".to_string()));
                }
            }
        }

        let mut kept = 0u32;
        let mut line = Vec::new();
        for (label, path, stand, face, ok, cmds) in actions {
            let mut probe = rt.clone();
            let (mut out, mut end) = run_idle(ctx, &mut probe, PROBE_FRAMES, 60, &path, stand, face, ok);
            // A question that led nowhere with the run's answer: try the others (mrkt3's fatman1 sends Cloud and
            // Aerith on to the dress shop only on the second answer). The first that moves the story or the field
            // becomes this action's answer, and the run's from then on.
            let mut trial = None;
            if story_key(&probe.mem.banks) == story_key(&rt.mem.banks) && scratch_key(&probe) == scratch_key(&baseline) && out.jump.is_none() {
                'asks: for (dialog, count) in out.asks.clone() {
                    let current = ctx.answer(dialog, (0, count.saturating_sub(1))).unwrap_or(0);
                    for c in (0..count).filter(|c| *c != current) {
                        let pb = Playback { field: ctx.field, choices: ctx.choices, trial: Some((u16::from(dialog), c)) };
                        let mut p2 = rt.clone();
                        let (o2, e2) = run_idle(&pb, &mut p2, PROBE_FRAMES, 60, &path, stand, face, ok);
                        if story_key(&p2.mem.banks) != story_key(&rt.mem.banks) || o2.jump.is_some() {
                            probe = p2;
                            out = o2;
                            end = e2;
                            trial = Some((u16::from(dialog), c));
                            break 'asks;
                        }
                    }
                }
            }
            let trial_ctx = Playback { field: ctx.field, choices: ctx.choices, trial };
            let ctx = &trial_ctx;
            let moved_story = story_key(&probe.mem.banks) != story_key(&rt.mem.banks);
            let moved_scratch = scratch_key(&probe) != scratch_key(&baseline);
            if (moved_story || moved_scratch || out.windows > 0 || probe.player_scripted()) && out.jump.is_none() && !out.game_over {
                let (more, e) = run_idle(ctx, &mut probe, FOLLOW_FRAMES, 30, &[], end, face, false);
                out.absorb(more);
                end = e;
            }
            if out.game_over {
                continue;
            }
            let after = moment_of(&probe.mem.banks);
            let moved_story = story_key(&probe.mem.banks) != story_key(&rt.mem.banks);
            let moved_scratch = scratch_key(&probe) != scratch_key(&baseline);
            // Somewhere the player could not have walked to (a jump, a ladder, a door a script opened).
            let end_place = place_of(&probe, end);
            let moved_place = out.jump.is_none() && end_place != here_place && end_place.1 > 0;
            let score = match (out.jump.is_some(), after > now, moved_story) {
                (true, true, _) => 5,
                (false, true, _) => 4,
                (true, false, _) => 3,
                (false, false, true) => 2,
                _ if moved_scratch || moved_place => 1,
                _ => 0,
            };
            if std::env::var_os("FMV_EXPLORE_TALK").is_some() && label.starts_with("talk") {
                println!("      probe [{label}] face {face}: windows {} moment {now}->{after} score {score}", out.windows);
            }
            if score == 0 {
                continue;
            }
            if let Some((dialog, choice)) = trial {
                println!("      answer {dialog} with choice {choice} in {field_name} ({label})");
                discovered.push((field_name.clone(), dialog, choice));
            }
            // The probe stood there out of nowhere. Now do it the way a player does: walk there from where the player
            // stands, and take whatever happens on the way instead if something does.
            let debug = std::env::var_os("FMV_EXPLORE_DEBUG").is_some();
            let first = path.first().copied().unwrap_or(stand);
            let Some(first_tri) = rt.triangle_at(first.0, first.1, first.2).map(usize::from) else {
                if debug { println!("      drop [{label}]: its start is on no triangle"); }
                continue;
            };
            let Some((approach, legs)) = way(&walk, &rt, at, here, first, first_tri) else {
                if debug { println!("      drop [{label}]: no way from tri {here} to tri {first_tri}"); }
                continue;
            };
            let mut probe = rt.clone();
            let mut out = Outcome::default();
            let (arrived, mut end) = walk_to(ctx, &mut probe, &approach, at, &mut out);
            if !arrived && out.jump.is_none() && !out.control_taken {
                // The exported corners still contain the unused movement tail. A dialogue/scratch change alone
                // does not cancel that tail over TCP, so the bot would idle at a different position.
                if debug { println!("      drop [{label}]: walk stopped without the bot cancelling its movement tail"); }
                continue;
            }
            if arrived && out.control_taken && !cmds.is_empty() {
                // A scene on the final approach step can return to the same place. The bot has already discarded
                // this entry's pending action, even if the solver considers the approach complete.
                if debug { println!("      drop [{label}]: scene cancelled the pending action at the end of the walk"); }
                continue;
            }
            let mut label = label;
            // The bot walks the same corners, then does what the action does there.
            let mut cmds = [legs.as_str(), cmds.as_str()].iter().filter(|c| !c.is_empty()).cloned().collect::<Vec<_>>().join("; ");
            if arrived {
                let (more, e) = run_idle(ctx, &mut probe, PROBE_FRAMES, 60, &path, stand, face, ok);
                out.absorb(more);
                end = e;
            } else {
                label = format!("walking towards [{label}], stopped at {},{}", end.0, end.1);
                cmds = legs.clone();
            }
            if out.jump.is_none() && !out.game_over && (busy(&probe) || !arrived || out.windows > 0 || story_key(&probe.mem.banks) != story_key(&rt.mem.banks) || scratch_key(&probe) != scratch_key(&baseline)) {
                let (more, e) = run_idle(ctx, &mut probe, FOLLOW_FRAMES, 30, &[], end, face, false);
                out.absorb(more);
                end = e;
            }
            if out.game_over {
                continue;
            }
            let Some(cmds) = out.idle.commands(cmds) else {
                if debug { println!("      drop [{label}]: player action after voluntary idle; no faithful suffix export"); }
                continue;
            };
            let after = moment_of(&probe.mem.banks);
            let moved_story = story_key(&probe.mem.banks) != story_key(&rt.mem.banks);
            let moved_scratch = scratch_key(&probe) != scratch_key(&baseline);
            let end_place = place_of(&probe, end);
            let moved_place = out.jump.is_none() && end_place != here_place && end_place.1 > 0;
            // A script that took the walk over and put the player somewhere else (chrin_2's jump down from triangles
            // 118/119 to 110) is progress even on a walkmesh of one part: the place it leaves the player is new.
            let relocated = out.jump.is_none() && !arrived && out.control_taken && f64::from(end.0 - at.0).hypot(f64::from(end.1 - at.1)) > 100.0;
            let score = match (out.jump.is_some(), after > now, moved_story) {
                (true, true, _) => 5,
                (false, true, _) => 4,
                (true, false, _) => 3,
                (false, false, true) => 2,
                _ if moved_scratch || moved_place || relocated => 1,
                _ => 0,
            };
            if score == 0 {
                if debug { println!("      drop [{label}]: walked (arrived {arrived}), nothing changed; ends {end:?}"); }
                continue;
            }
            let summary = format!("{field_name}: {label} (moment {now}->{after}, battles {:?}, movies {:?}, windows {})", out.battles, out.movies, out.windows);
            match out.jump {
                Some(jump) => {
                    let to = maplist.name(jump.0).unwrap_or("?").to_string();
                    if entered.contains(&(to.clone(), story_key(&probe.mem.banks), i32::from(jump.3))) {
                        continue;
                    }
                    let via = Some(Rc::new(Step { text: format!("{summary} -> {to}"), cmds: format!("{field_name}: {cmds}"), parent: via.clone() }));
                    line.push(format!("[{score}] {label} -> {to}"));
                    push(&mut heap, enter(&to, jump, probe.mem.banks.clone(), via), score);
                }
                None => {
                    // A relocation is told apart by the triangle it ended on (its walkmesh part may be the same).
                    let spot = if relocated { probe.triangle_at(end.0, end.1, end.2).map_or(0, u64::from).wrapping_mul(0x2545_F491_4F6C_DD1D) } else { 0 };
                    if !images.insert((field_name.clone(), story_key(&probe.mem.banks), image_key(&probe, end_place) ^ spot)) {
                        continue;
                    }
                    let via = Some(Rc::new(Step { text: summary, cmds: format!("{field_name}: {cmds}"), parent: via.clone() }));
                    line.push(format!("[{score}] {label}"));
                    let banks = probe.mem.banks.clone();
                    let tri = probe.triangle_at(end.0, end.1, end.2).map(usize::from);
                    push(&mut heap, State { field: field_name.clone(), rt: Some(probe), banks, pos: Some(end), tri, via, carry: None }, score);
                }
            }
            kept += 1;
        }
        // Gateways the player can reach, while the scripts allow map jumps.
        let mut gates = 0;
        if rt.state.map_jumps_disabled && std::env::var_os("FMV_EXPLORE_DEBUG").is_some() {
            println!("      no gates: the scripts disabled map jumps");
        }
        if !rt.state.map_jumps_disabled {
            let gateways: Vec<Gateway> = loaded.field.triggers().map(|t| t.gateways).unwrap_or_default();
            for (gi, g) in gateways.iter().enumerate().filter(|(_, g)| g.target_field_id != Gateway::UNUSED_FIELD_ID) {
                let mid = ((i32::from(g.exit_a[0]) + i32::from(g.exit_b[0])) / 2, (i32::from(g.exit_a[1]) + i32::from(g.exit_b[1])) / 2, (i32::from(g.exit_a[2]) + i32::from(g.exit_b[2])) / 2);
                let near = [(0, 0), (-30, 0), (30, 0), (0, -30), (0, 30), (-60, 0), (60, 0), (0, -60), (0, 60)].iter().any(|(ox, oy)| on_reach(&rt, (mid.0 + ox, mid.1 + oy, mid.2)));
                let Some(to) = maplist.name(g.target_field_id).map(str::to_string) else { continue };
                if !near || entered.contains(&(to.clone(), story_key(&rt.mem.banks), i32::from(g.destination[2]))) {
                    if std::env::var_os("FMV_EXPLORE_DEBUG").is_some() {
                        println!("      drop [gate {gi}] -> {to}: {}", if near { "already entered with this story" } else { "not near the reachable walkmesh" });
                    }
                    continue;
                }
                // Walk to the gate; if a script steps in on the way, that is what happens instead of the gate.
                let spot = [(0, 0), (-30, 0), (30, 0), (0, -30), (0, 30), (-60, 0), (60, 0), (0, -60), (0, 60)].iter().map(|(ox, oy)| (mid.0 + ox, mid.1 + oy, mid.2)).find(|p| on_reach(&rt, *p));
                let Some(spot) = spot else { continue };
                let Some(spot_tri) = rt.triangle_at(spot.0, spot.1, spot.2).map(usize::from) else { continue };
                // The direct way, and one that keeps clear of active lines when that is a different way.
                let mut plans: Vec<(Vec<Pos>, String)> = way(&walk, &rt, at, here, spot, spot_tri).into_iter().collect();
                if let Some(clear) = way_with(&walk, &rt, at, here, spot, spot_tri, Some(&active_lines(&rt))) {
                    if !plans.iter().any(|p| p.1 == clear.1) {
                        plans.push(clear);
                    }
                }
                if plans.is_empty() {
                    if std::env::var_os("FMV_EXPLORE_DEBUG").is_some() {
                        println!("      drop [gate {gi}] -> {to}: no walk from tri {here} to tri {spot_tri}");
                    }
                    continue;
                }
                for (mut approach, legs) in plans {
                    if std::env::var_os("FMV_EXPLORE_DEBUG").is_some() {
                        println!("      plan [gate {gi}] from {at:?}: {legs}");
                    }
                approach.extend(walk_segment(approach.last().copied().unwrap_or(at), mid));
                let mut probe = rt.clone();
                let mut out = Outcome::default();
                let (arrived, end) = walk_to(ctx, &mut probe, &approach, at, &mut out);
                if !arrived {
                    if out.jump.is_none() && !out.control_taken {
                        if std::env::var_os("FMV_EXPLORE_DEBUG").is_some() {
                            println!("      drop [gate {gi}]: walk stopped without the bot cancelling its movement tail (from {at:?} tri {here} to {mid:?}, stopped at {end:?}, path {approach:?}, lock {}, map jumps off {})", probe.state.character_lock, probe.state.map_jumps_disabled);
                            for (i, m) in probe.models().iter().enumerate() {
                                println!("        model {i} entity {} at ({},{},{}) solid_off {} visible {} range {}", m.entity, m.x, m.y, m.z, m.solid_off, m.visible, m.solid_range);
                            }
                        }
                        continue;
                    }
                    let (more, end) = if out.jump.is_none() && !out.game_over { run_idle(ctx, &mut probe, FOLLOW_FRAMES, 30, &[], end, 0, false) } else { (Outcome::default(), end) };
                    out.absorb(more);
                    if out.game_over {
                        continue;
                    }
                    let after = moment_of(&probe.mem.banks);
                    let label = format!("walking towards gate {gi}, stopped at {},{}", end.0, end.1);
                    let summary = format!("{field_name}: {label} (moment {now}->{after}, battles {:?}, movies {:?}, windows {})", out.battles, out.movies, out.windows);
                    let Some(cmds) = out.idle.commands(format!("{legs}; to {} {}", mid.0, mid.1)) else {
                        if std::env::var_os("FMV_EXPLORE_DEBUG").is_some() {
                            println!("      drop [gate {gi}]: player action after voluntary idle; no faithful suffix export");
                        }
                        continue;
                    };
                    let cmds = format!("{field_name}: {cmds}");
                    let score = if after > now { 4 } else { 2 };
                    match out.jump {
                        Some(jump) => {
                            let to = maplist.name(jump.0).unwrap_or("?").to_string();
                            if !entered.contains(&(to.clone(), story_key(&probe.mem.banks), i32::from(jump.3))) {
                                line.push(format!("[{score}] {label} -> {to}"));
                                push(&mut heap, enter(&to, jump, probe.mem.banks.clone(), Some(Rc::new(Step { text: format!("{summary} -> {to}"), cmds, parent: via.clone() }))), score + 1);
                            }
                        }
                        None => {
                            let end_place = place_of(&probe, end);
                            if images.insert((field_name.clone(), story_key(&probe.mem.banks), image_key(&probe, end_place))) {
                                line.push(format!("[{score}] {label}"));
                                let banks = probe.mem.banks.clone();
                                let tri = probe.triangle_at(end.0, end.1, end.2).map(usize::from);
                                push(&mut heap, State { field: field_name.clone(), rt: Some(probe), banks, pos: Some(end), tri, via: Some(Rc::new(Step { text: summary, cmds, parent: via.clone() })), carry: None }, score);
                            }
                        }
                    }
                    continue;
                }
                if out.idle.frames > 0 || out.idle.action_after_idle {
                    // The bot's gateway command walks across it. A pause before that movement needs an interleaved
                    // trace, not an idle suffix after the gateway (which a field change would discard).
                    if std::env::var_os("FMV_EXPLORE_DEBUG").is_some() {
                        println!("      drop [gate {gi}]: voluntary idle before gateway movement needs an interleaved export");
                    }
                    continue;
                }
                let jump = (g.target_field_id, g.destination[0], g.destination[1], g.destination[2]);
                let via = Some(Rc::new(Step { text: format!("{field_name}: gate {gi} (moment {now}) -> {to}"), cmds: format!("{field_name}: {}gate {gi}", if legs.is_empty() { String::new() } else { format!("{legs}; ") }), parent: via.clone() }));
                let score = if visited.contains(&to) { 0 } else { 3 };
                line.push(format!("[{score}] gate {gi} -> {to}"));
                push(&mut heap, enter(&to, jump, probe.mem.banks.clone(), via), score);
                gates += 1;
                }
            }
        }
        line.sort_by(|a, b| b.cmp(a));
        println!(
            "{step:>4} {field_name:<10} moment {now} tri {here} reach {reach_count}/{}: {kept} actions, {gates} gates{}",
            walk.triangles.len(),
            if line.is_empty() { String::new() } else { format!(": {}", line.iter().take(4).cloned().collect::<Vec<_>>().join("; ")) }
        );
    }
    println!("\nfurthest: moment {} in {} ({} expansions; other modes met: {unloadable:?})", best.0, best.2, step);
    let route = chain(&best.1);
    for l in &route {
        println!("  {l}");
    }
    if let Some(target) = until.as_ref().filter(|_| !reached_target) {
        eprintln!("target not reached: {target}; no target route written");
        std::process::exit(1);
    }
    if let Some(path) = route_out {
        let mut cmds = choices.headers();
        cmds.extend(commands(&best.1));
        if let Some(target) = until {
            // Even a route made entirely of automatic scenes must wait for its destination over TCP.
            cmds.push(format!("{target}: wait 0"));
        }
        let _ = std::fs::write(path, cmds.join("\n") + "\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two synthetic entities: a director requests the worker at priority 6, so its voluntary waits remain
    /// observable to run_idle's settling logic. One empty synthetic message; no owned game data.
    fn runtime(worker: &[u8]) -> FieldRuntime {
        let code_start = 0x20 + 2 * (8 + 64);
        let mut code = Vec::new();
        let mut entries = [[0u16; 32]; 2];
        for (entity, row) in entries.iter_mut().enumerate() {
            for (slot, entry) in row.iter_mut().enumerate() {
                *entry = (code_start + code.len()) as u16;
                match (entity, slot) {
                    (0, 0) => code.extend_from_slice(&[0, 3, 1, 0xC1, 0]), // RET | REQEW worker.1; RET
                    (1, 0) => code.extend_from_slice(&[0, 0]),
                    (1, 1) => code.extend_from_slice(worker),
                    _ => code.push(0),
                }
            }
        }
        let mut bytes = vec![0; 0x20];
        bytes[0..3].copy_from_slice(&[2, 5, 2]);
        bytes[4..6].copy_from_slice(&((code_start + code.len()) as u16).to_le_bytes());
        bytes[8..10].copy_from_slice(&512u16.to_le_bytes());
        bytes.extend_from_slice(b"directorworker\0\0");
        for row in entries {
            for entry in row {
                bytes.extend_from_slice(&entry.to_le_bytes());
            }
        }
        bytes.extend(code);
        bytes.extend_from_slice(&[1, 0, 4, 0, 0xFF]); // one message, empty text
        let mut rt = FieldRuntime::new(&bytes, Banks::default()).unwrap();
        rt.init();
        rt
    }

    fn scene(lock_frames: u16) -> FieldRuntime {
        let [lo, hi] = lock_frames.to_le_bytes();
        runtime(&[
            0x33, 1, 0x24, lo, hi, // lock and wait
            0x40, 0, 0, // message
            0x33, 0, 0x24, 60, 0, // unlock and voluntarily stand for 60 frames
            0x80, 0x10, 0x30, 1, 0, // set a flag, return
        ])
    }

    #[test]
    fn forced_scene_length_does_not_become_exported_idle() {
        let choices = ChoicePolicy::default();
        let ctx = &Playback { field: "fixture", choices: &choices, trial: None };
        let (mut short, mut long) = (scene(30), scene(90));
        let (a, _) = run_idle(ctx, &mut short, 500, 0, &[], (0, 0, 0), 0, false);
        let (b, _) = run_idle(ctx, &mut long, 500, 0, &[], (0, 0, 0), 0, false);
        assert_eq!((short.mem.banks.get(0x30), long.mem.banks.get(0x30)), (1, 1));
        assert_eq!((a.windows, b.windows), (1, 1));
        assert_eq!(long.frame() - short.frame(), 60);
        assert_eq!(a.idle.frames, b.idle.frames);
        assert!(a.idle.frames >= 60);
        let exported = a.idle.commands("to 10 0".to_string()).unwrap();
        assert_eq!(exported.matches("idle").count(), 1);
        assert_eq!(exported, b.idle.commands("to 10 0".to_string()).unwrap());
    }

    #[test]
    fn a_later_forced_scene_pauses_the_idle_suffix() {
        let choices = ChoicePolicy::default();
        let ctx = &Playback { field: "fixture", choices: &choices, trial: None };
        let body = |locked: u8| vec![
            0x24, 30, 0, // voluntary wait
            0x33, 1, 0x24, locked, 0, 0x33, 0, // forced scene
            0x24, 30, 0, 0, // voluntary wait, return
        ];
        let (mut short, mut long) = (runtime(&body(30)), runtime(&body(90)));
        let (a, _) = run_idle(ctx, &mut short, 500, 0, &[], (0, 0, 0), 0, false);
        let (b, _) = run_idle(ctx, &mut long, 500, 0, &[], (0, 0, 0), 0, false);
        assert_eq!(long.frame() - short.frame(), 60);
        assert_eq!(a.idle.frames, b.idle.frames);
        assert_eq!(a.idle.commands(String::new()), b.idle.commands(String::new()));
    }

    #[test]
    fn movement_after_an_internal_pause_is_not_exported_at_the_wrong_time() {
        let choices = ChoicePolicy::default();
        let ctx = &Playback { field: "fixture", choices: &choices, trial: None };
        let mut rt = runtime(&[0]);
        let (mut out, at) = run_idle(ctx, &mut rt, 10, 0, &[], (0, 0, 0), 0, false);
        assert_eq!(out.idle.frames, 10);
        let (arrived, _) = walk_to(ctx, &mut rt, &[(6, 0, 0)], at, &mut out);
        assert!(arrived);
        assert!(out.idle.commands("to 6 0".to_string()).is_none());
    }

    #[test]
    fn ok_after_an_internal_pause_is_not_exported_before_the_pause() {
        let choices = ChoicePolicy::default();
        let ctx = &Playback { field: "fixture", choices: &choices, trial: None };
        let mut rt = runtime(&[0]);
        let (mut out, at) = run_idle(ctx, &mut rt, 10, 0, &[], (0, 0, 0), 0, false);
        let (more, _) = run_idle(ctx, &mut rt, 7, 0, &[], at, 0, true);
        assert_eq!(more.idle.frames, 0);
        out.absorb(more);
        assert!(out.idle.commands("ok".to_string()).is_none());
    }

    #[test]
    fn ordinary_dialogue_does_not_mark_an_interrupted_walk_as_cancelled_by_the_bot() {
        let choices = ChoicePolicy::default();
        let ctx = &Playback { field: "fixture", choices: &choices, trial: None };
        for locked in [false, true] {
            let mut body = vec![0x24, 1, 0];
            if locked { body.extend_from_slice(&[0x33, 1]); }
            body.extend_from_slice(&[0x40, 0, 0, 0x80, 0x10, 0x30, 1, 0x33, 0, 0]);
            let mut rt = runtime(&body);
            let mut out = Outcome::default();
            let (arrived, end) = walk_to(ctx, &mut rt, &walk_segment((0, 0, 0), (200, 0, 0)), (0, 0, 0), &mut out);
            assert!(!arrived);
            assert_ne!(end, (200, 0, 0));
            assert_eq!(out.windows, 1);
            assert_eq!(out.control_taken, locked);
        }
    }

    #[test]
    fn walking_matches_two_hundred_units_per_second_without_recording_path_frames_as_idle() {
        let choices = ChoicePolicy::default();
        let ctx = &Playback { field: "fixture", choices: &choices, trial: None };
        let path = walk_segment((0, 0, 0), (200, 0, 30));
        assert_eq!(path.len(), 30);
        assert_eq!(path.last(), Some(&(200, 0, 30)));
        assert!(walk_segment((0, 0, 0), (0, 0, 0)).is_empty());
        let mut rt = runtime(&[0]);
        let crossing = [(0, 0, 0), (6, 0, 0), (13, 0, 0)];
        let (out, _) = run_idle(ctx, &mut rt, 3, 0, &crossing, (0, 0, 0), 0, false);
        assert_eq!(out.idle.frames, 0);
        assert!(out.idle.commands("to 13 0".to_string()).is_some());
    }

    #[test]
    fn choice_directives_reject_malformed_and_duplicate_values() {
        for directive in ["", "fixture:0", ":0=1", "fixture:=1", "fixture:0=", "fixture:0=256", "fixture:65536=0",
            "fixture:0=-1", "fixture:+1=0", "fixture:0=+1", "fixture:0=1=2", "fixture:1:2=0", "bad-field:0=0", "é:0=0", "field name:0=0", "fixture:0=1 "] {
            assert!(ChoicePolicy::default().insert(directive).is_err(), "{directive:?}");
        }
        let mut policy = ChoicePolicy::default();
        policy.insert("fixture:0=2").unwrap();
        assert!(policy.insert("fixture:0=2").unwrap_err().contains("duplicate"));
        assert!(policy.insert("fixture:00=1").unwrap_err().contains("duplicate"));
        assert_eq!(policy.headers(), ["@choice fixture 0 2"]);
        policy.insert("other_1:65535=255").unwrap();
        assert_eq!(policy.headers(), ["@choice fixture 0 2", "@choice other_1 65535 255"]);
    }

    #[test]
    fn entry_scene_choices_are_scoped_to_field_and_dialog_across_visits() {
        let mut choices = ChoicePolicy::default();
        choices.insert("fixture:0=2").unwrap();
        choices.insert("fixture:1=1").unwrap();
        for (field, expected) in [("fixture", 2), ("other", 0), ("fixture", 2)] {
            let ctx = &Playback { field, choices: &choices, trial: None };
            // An automatic worker asks dialog 0 with authored choice lines 1..3, storing the relative index in 5:6.
            let mut rt = runtime(&[0x48, 0x05, 0, 0, 1, 3, 6, 0]);
            let (out, _) = run_idle(ctx, &mut rt, 100, 0, &[], (0, 0, 0), 0, false);
            assert_eq!(out.windows, 1);
            assert_eq!(rt.mem.scratch[6], expected);
            assert!(!rt.window_waiting());
        }
        let ctx = Playback { field: "fixture", choices: &choices, trial: None };
        assert_eq!(ctx.answer(1, (4, 6)), Ok(1));
        assert_eq!(ctx.answer(2, (4, 6)), Ok(0));
    }

    #[test]
    fn configured_choice_outside_encountered_question_is_an_error() {
        let mut choices = ChoicePolicy::default();
        choices.insert("fixture:14=2").unwrap();
        let ctx = Playback { field: "fixture", choices: &choices, trial: None };
        assert_eq!(ctx.answer(14, (1, 3)), Ok(2));
        let error = ctx.answer(14, (1, 2)).unwrap_err();
        assert!(error.contains("fixture:14=2") && error.contains("out of range"));
        assert!(ctx.answer(14, (3, 1)).is_err());
        assert_eq!(Playback { field: "other", choices: &choices, trial: None }.answer(14, (1, 2)), Ok(0));
        // A retried probe answers one question otherwise, and only that one.
        let pb = Playback { field: "other", choices: &choices, trial: Some((14, 1)) };
        assert_eq!((pb.answer(14, (1, 2)), pb.answer(15, (1, 2))), (Ok(1), Ok(0)));
    }
}
