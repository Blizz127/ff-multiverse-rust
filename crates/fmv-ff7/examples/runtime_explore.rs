//! Play the story headlessly on the live field runtime, field after field, and say how far it gets (needs the owner's
//! data). The route solver behind `docs/disc1-live-route.txt`.
//!
//! ```text
//! FMV_FF7_DATA=<.../data> cargo run --release -p fmv-ff7 --example runtime_explore -- \
//!     [--from FIELD] [--tri N] [--moment M] [--set SEL:ADDR=VALUE]... [--steps N] [--route-out FILE]
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
//! first choice, battles count as won, movies, menus and tutorials complete at once.
//!
//! It prints one line per expansion and, at the end, the chain of actions that led to the furthest moment. Numbers
//! and names only, never game text. Standing on a triangle's centre is a test shortcut: solid models and exact
//! collision are not modelled, so a route it finds is a strong lead, not a proof.
#[path = "../tests/script_common.rs"]
mod common;

use common::{data_dir, flevel_path, load_field};
use fmv_ff7::field::{FieldFile, Gateway, Walkmesh};
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::runtime::{direction_of, Banks, Command, Event, FieldRuntime, PlayerInput, PERSISTENT_BYTES};
use fmv_ff7::script::Maplist;
use fmv_ff7::story::StoryState;
use std::collections::{BTreeMap, BinaryHeap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

const SETTLE_FRAMES: u32 = 900;
const PROBE_FRAMES: u32 = 300;
const FOLLOW_FRAMES: u32 = 3600;
/// Expansions allowed per (field, moment): a field whose actions only shuffle its scratch must not eat the search.
const PER_PLACE: u32 = 10;
/// The pad's OK bit.
const OK: u16 = 0x20;

type Pos = (i32, i32, i32);

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
}

impl Outcome {
    fn absorb(&mut self, more: Outcome) {
        self.jump = more.jump;
        self.battles.extend(more.battles);
        self.movies.extend(more.movies);
        self.windows += more.windows;
        self.game_over |= more.game_over;
    }
}

/// Run `frames` frames. The player walks `path` (a point a frame), then stands at its end, facing `dir`, holding OK
/// on frames 2 to 6 when `ok`. While a script holds the player, the player follows where the scripts put it. Returns
/// what happened and where the player ends.
fn run(rt: &mut FieldRuntime, frames: u32, path: &[Pos], mut at: Pos, dir: u8, ok: bool) -> (Outcome, Pos) {
    let mut out = Outcome::default();
    for f in 0..frames {
        if let Some(p) = path.get(f as usize) {
            at = *p;
        }
        if rt.player_scripted() {
            if let Some(m) = rt.pc_model() {
                at = (m.x, m.y, m.z);
            }
        }
        let held = ok && (2..=6).contains(&f);
        let player = PlayerInput { x: at.0, y: at.1, z: at.2, dir, ok: ok && f == 2, keys_held: if held { OK } else { 0 }, keys_pressed: if ok && f == 2 { OK } else { 0 }, ..Default::default() };
        rt.tick(Some(player));
        if rt.pending_tutorial().is_some() {
            rt.complete_tutorial();
        }
        for ev in rt.drain_events() {
            match ev {
                Event::Window { window, ask, .. } => {
                    out.windows += 1;
                    if ask.is_some() {
                        rt.answer(window, 0);
                    } else {
                        rt.close_window(window);
                    }
                }
                Event::Command(Command::MapJump { field, x, y, triangle, .. }) | Event::Command(Command::Minigame { field, x, y, triangle, .. }) => {
                    out.jump = Some((field, x, y, triangle));
                    return (out, at);
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
                    return (out, at);
                }
                Event::Command(_) => rt.complete_command(),
                _ => {}
            }
        }
    }
    if rt.player_scripted() {
        if let Some(m) = rt.pc_model() {
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

/// The triangles the player can walk to from `from` without entering a locked one.
fn reachable(walk: &Walkmesh, rt: &FieldRuntime, from: usize) -> Vec<bool> {
    let mut seen = vec![false; walk.triangles.len()];
    if from >= seen.len() {
        return seen;
    }
    seen[from] = true;
    let mut queue = VecDeque::from([from]);
    while let Some(t) = queue.pop_front() {
        for &n in &walk.access[t] {
            let n = usize::from(n);
            if n < seen.len() && !seen[n] && !rt.triangle_locked(n as u16) {
                seen[n] = true;
                queue.push_back(n);
            }
        }
    }
    seen
}

/// One step of a route.
struct Step {
    text: String,
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
}

fn main() {
    let Some(dir) = data_dir() else {
        eprintln!("FMV_FF7_DATA is not set; skipping");
        return;
    };
    let mut args = std::env::args().skip(1);
    let (mut start_field, mut steps, mut start_moment, mut route_out) = ("md1stin".to_string(), 200u32, None::<u16>, None::<String>);
    let mut sets: Vec<(u8, u8, u8)> = Vec::new();
    let mut start_tri: Option<usize> = None;
    while let Some(a) = args.next() {
        let v = args.next().unwrap_or_default();
        match a.as_str() {
            "--from" => start_field = v,
            "--steps" => steps = v.parse().unwrap_or(steps),
            "--moment" => start_moment = v.parse().ok(),
            "--route-out" => route_out = Some(v),
            "--tri" => start_tri = v.parse().ok(),
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
    let mut lgp = LgpArchive::open_path(&flevel_path(&dir)).expect("flevel");
    let maplist = Maplist::from_archive(&mut lgp).expect("maplist");
    let mut banks = Banks::from_story(&StoryState::fresh_reset()).expect("complete");
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
                Some(Rc::new(Loaded { field, walk }))
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
    let mut entered: HashSet<(String, u64)> = HashSet::new();
    let mut visited: HashSet<String> = HashSet::new();
    let mut per_place: BTreeMap<(String, u16), u32> = BTreeMap::new();
    let mut best: (u16, Option<Rc<Step>>, String) = (0, None, String::new());
    let mut unloadable: Vec<String> = Vec::new();

    let mut step = 0u32;
    while step < steps {
        let Some(Queued { state, .. }) = heap.pop() else {
            println!("nothing left to try");
            break;
        };
        let State { field: field_name, rt, banks, pos, tri, via, carry } = state;
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
                if !entered.insert((field_name.clone(), story_key(&banks))) {
                    continue;
                }
                let Ok(mut rt) = FieldRuntime::new(section, banks.clone()) else { continue };
                rt.set_walkmesh(Arc::clone(&walk));
                if let Some((save, gil)) = &carry {
                    rt.set_save_data(save.clone());
                    rt.gil = *gil;
                }
                rt.init();
                rt
            }
        };
        let place = (field_name.clone(), moment_of(&rt.mem.banks));
        let count = per_place.entry(place).or_insert(0);
        if *count >= PER_PLACE {
            continue;
        }
        *count += 1;
        step += 1;
        visited.insert(field_name.clone());

        // Where the player stands: where the state says, else where the field's own scripts put its character.
        let mut at = pos.or_else(|| rt.pc_model().map(|m| (m.x, m.y, m.z))).unwrap_or_else(|| centre(&walk, 0));
        // (an arrival's height is its triangle's)
        if fresh {
            if let Some(t) = tri.filter(|t| *t < walk.triangles.len()) {
                at.2 = centre(&walk, t).2;
            }
        }
        let before = moment_of(&rt.mem.banks);
        let settle = if fresh { run(&mut rt, SETTLE_FRAMES, &[], at, 0, false) } else { (Outcome::default(), at) };
        let moved = settle.1 != at;
        at = settle.1;
        let now = moment_of(&rt.mem.banks);
        if now > best.0 {
            best = (now, via.clone(), field_name.clone());
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
            let via = Some(Rc::new(Step { text: format!("{field_name}: (plays by itself, moment {before}->{now}) -> {to}"), parent: via }));
            push(&mut heap, enter(&to, jump, rt.mem.banks.clone(), via), 5);
            continue;
        }
        let here = tri.filter(|_| fresh && !moved).or_else(|| rt.triangle_at(at.0, at.1, at.2).map(usize::from));
        let Some(here) = here.filter(|t| *t < walk.triangles.len()) else {
            println!("{step:>4} {field_name:<10} the player stands on no triangle at {at:?} (moment {now})");
            continue;
        };
        let reach = reachable(&walk, &rt, here);
        let reach_count = reach.iter().filter(|r| **r).count();

        // What the field does by itself over a probe's time, and which scratch bytes only follow the player's position.
        let mut baseline = rt.clone();
        let _ = run(&mut baseline, PROBE_FRAMES, &[], at, 0, false);
        let mut noise = [false; 256];
        for t in (0..walk.triangles.len()).filter(|t| reach[*t]).step_by((reach_count / 4).max(1)).take(5) {
            let mut other = rt.clone();
            let _ = run(&mut other, PROBE_FRAMES, &[], centre(&walk, t), 0, false);
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
        let mut actions: Vec<(String, Vec<Pos>, Pos, u8, bool)> = Vec::new();
        for t in (0..walk.triangles.len()).filter(|t| reach[*t]) {
            let c = centre(&walk, t);
            actions.push((format!("tri {t}"), Vec::new(), c, 0, false));
            actions.push((format!("tri {t}; ok"), Vec::new(), c, 0, true));
        }
        let on_reach = |rt: &FieldRuntime, p: Pos| rt.triangle_at(p.0, p.1, p.2).is_some_and(|t| reach.get(usize::from(t)).copied().unwrap_or(false));
        let pc = rt.state.pc_model;
        for (mi, m) in rt.models().iter().enumerate().filter(|(i, m)| Some(*i) != pc && !m.talk_off) {
            for (ox, oy) in [(-40, 0), (40, 0), (0, -40), (0, 40), (-28, -28), (28, 28), (-28, 28), (28, -28)] {
                let stand = (m.x + ox, m.y + oy, m.z);
                if on_reach(&rt, stand) {
                    let face = direction_of(i64::from(-ox), i64::from(-oy));
                    actions.push((format!("talk {} (model {mi}) from {},{}", rt.entity_name(m.entity).unwrap_or(""), stand.0, stand.1), Vec::new(), stand, face, true));
                    break;
                }
            }
        }
        for (li, l) in rt.lines().iter().enumerate().filter(|(_, l)| l.active) {
            let mid = ((i32::from(l.a.0) + i32::from(l.b.0)) / 2, (i32::from(l.a.1) + i32::from(l.b.1)) / 2, (i32::from(l.a.2) + i32::from(l.b.2)) / 2);
            let (dx, dy) = (f64::from(l.b.0 - l.a.0), f64::from(l.b.1 - l.a.1));
            let n = (dx * dx + dy * dy).sqrt().max(1.0);
            let (nx, ny) = (-dy / n, dx / n);
            let side = |k: i32| (mid.0 + (nx * 5.0 * f64::from(k)) as i32, mid.1 + (ny * 5.0 * f64::from(k)) as i32, mid.2);
            let (a_side, b_side) = (side(-6), side(6));
            let name = rt.entity_name(l.entity).unwrap_or("").to_string();
            if on_reach(&rt, a_side) {
                actions.push((format!("line {name} ({li}) across from {},{}", a_side.0, a_side.1), (-6..=6).map(side).collect(), b_side, 0, false));
            }
            if on_reach(&rt, b_side) {
                actions.push((format!("line {name} ({li}) across from {},{}", b_side.0, b_side.1), (-6..=6).rev().map(side).collect(), a_side, 0, false));
            }
            // OK at the line: stand a few units off it on a reachable side, facing it (the original wants the line
            // in front of the player), or on its middle when neither side can be stood on.
            let mut stood = false;
            for k in [-2, 2, -4, 4] {
                let stand = side(k);
                if on_reach(&rt, stand) {
                    let face = direction_of((-nx * f64::from(k) * 100.0) as i64, (-ny * f64::from(k) * 100.0) as i64);
                    actions.push((format!("line {name} ({li}); ok from {},{} facing {face}", stand.0, stand.1), Vec::new(), stand, face, true));
                    stood = true;
                }
            }
            if !stood && on_reach(&rt, mid) {
                actions.push((format!("line {name} ({li}); ok at {},{}", mid.0, mid.1), Vec::new(), mid, 0, true));
            }
        }

        let mut kept = 0u32;
        let mut line = Vec::new();
        for (label, path, stand, face, ok) in actions {
            let mut probe = rt.clone();
            let (mut out, mut end) = run(&mut probe, PROBE_FRAMES, &path, stand, face, ok);
            let moved_story = story_key(&probe.mem.banks) != story_key(&rt.mem.banks);
            let moved_scratch = scratch_key(&probe) != scratch_key(&baseline);
            if (moved_story || moved_scratch || out.windows > 0 || probe.player_scripted()) && out.jump.is_none() && !out.game_over {
                let (more, e) = run(&mut probe, FOLLOW_FRAMES, &[], end, face, false);
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
            if score == 0 {
                continue;
            }
            let summary = format!("{field_name}: {label} (moment {now}->{after}, battles {:?}, movies {:?}, windows {})", out.battles, out.movies, out.windows);
            match out.jump {
                Some(jump) => {
                    let to = maplist.name(jump.0).unwrap_or("?").to_string();
                    if entered.contains(&(to.clone(), story_key(&probe.mem.banks))) {
                        continue;
                    }
                    let via = Some(Rc::new(Step { text: format!("{summary} -> {to}"), parent: via.clone() }));
                    line.push(format!("[{score}] {label} -> {to}"));
                    push(&mut heap, enter(&to, jump, probe.mem.banks.clone(), via), score);
                }
                None => {
                    if !images.insert((field_name.clone(), story_key(&probe.mem.banks), image_key(&probe, end_place))) {
                        continue;
                    }
                    let via = Some(Rc::new(Step { text: summary, parent: via.clone() }));
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
        if !rt.state.map_jumps_disabled {
            let gateways: Vec<Gateway> = loaded.field.triggers().map(|t| t.gateways).unwrap_or_default();
            for (gi, g) in gateways.iter().enumerate().filter(|(_, g)| g.target_field_id != Gateway::UNUSED_FIELD_ID) {
                let mid = ((i32::from(g.exit_a[0]) + i32::from(g.exit_b[0])) / 2, (i32::from(g.exit_a[1]) + i32::from(g.exit_b[1])) / 2, (i32::from(g.exit_a[2]) + i32::from(g.exit_b[2])) / 2);
                let near = [(0, 0), (-30, 0), (30, 0), (0, -30), (0, 30), (-60, 0), (60, 0), (0, -60), (0, 60)].iter().any(|(ox, oy)| on_reach(&rt, (mid.0 + ox, mid.1 + oy, mid.2)));
                let Some(to) = maplist.name(g.target_field_id).map(str::to_string) else { continue };
                if !near || entered.contains(&(to.clone(), story_key(&rt.mem.banks))) {
                    continue;
                }
                let jump = (g.target_field_id, g.destination[0], g.destination[1], g.destination[2]);
                let via = Some(Rc::new(Step { text: format!("{field_name}: gate {gi} (moment {now}) -> {to}"), parent: via.clone() }));
                let score = if visited.contains(&to) { 0 } else { 3 };
                line.push(format!("[{score}] gate {gi} -> {to}"));
                push(&mut heap, enter(&to, jump, rt.mem.banks.clone(), via), score);
                gates += 1;
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
    if let Some(path) = route_out {
        let _ = std::fs::write(path, route.join("\n") + "\n");
    }
}
