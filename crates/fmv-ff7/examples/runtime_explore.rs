//! Play the story headlessly on the live field runtime, field after field, and say how far it gets (needs the owner's
//! data). The route solver behind `docs/disc1-live-route.txt`.
//!
//! ```text
//! FMV_FF7_DATA=<.../data> cargo run --release -p fmv-ff7 --example runtime_explore -- [--from FIELD] [--steps N] [--moment M]
//! ```
//!
//! From a fresh story (every bit known) in `md1stin`, it repeats:
//!
//! 1. run the field until it settles (windows answered with their first choice, battles counted as won, movies, menus and
//!    tutorials completed); a map jump takes it to the next field with the story as it is;
//! 2. otherwise try every walkmesh triangle on a copy of the field: stand on its centre, then also press OK, and see
//!    what the scripts do (the story moment moves, a map jump, a battle);
//! 3. take the best action (a map jump that moves the story, then one that moves it at all, then any map jump) and go on.
//!
//! It prints one line per action. Numbers and names only, never game text. Teleporting to a triangle's centre misses
//! triggers that need a walk across a line; those fields stop the solver and are reported.
#[path = "../tests/script_common.rs"]
mod common;

use common::{data_dir, flevel_path, load_field};
use fmv_ff7::field::FieldFile;
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::runtime::{Banks, Command, Event, FieldRuntime, PlayerInput};
use fmv_ff7::script::Maplist;
use fmv_ff7::story::StoryState;
use std::sync::Arc;

const SETTLE_FRAMES: u32 = 900;
const PROBE_FRAMES: u32 = 360;

/// Whether the story (persistent bytes the engine does not own) differs between two runtimes.
fn story_changed(a: &FieldRuntime, b: &FieldRuntime) -> bool {
    (0..fmv_ff7::runtime::PERSISTENT_BYTES).any(|i| {
        let sel = [1u8, 3, 11, 13, 15][i / 256];
        let mask = !fmv_ff7::story::engine_owned_bits(sel, (i % 256) as u8);
        (a.mem.banks.get(i) ^ b.mem.banks.get(i)) & mask != 0
    })
}

/// A fingerprint of the story a runtime holds (persistent bytes the engine does not own).
fn story_key(rt: &FieldRuntime) -> u64 {
    banks_key(&rt.mem.banks)
}

fn banks_key(banks: &Banks) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for i in 0..fmv_ff7::runtime::PERSISTENT_BYTES {
        let sel = [1u8, 3, 11, 13, 15][i / 256];
        let b = banks.get(i) & !fmv_ff7::story::engine_owned_bits(sel, (i % 256) as u8);
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

fn moment(rt: &FieldRuntime) -> u16 {
    u16::from_le_bytes([rt.mem.banks.get(0), rt.mem.banks.get(1)])
}

#[derive(Debug, Clone, Default)]
struct Outcome {
    jump: Option<(u16, i16, i16, i16, u8)>,
    battles: Vec<u16>,
    windows: u32,
    movies: Vec<u16>,
}

/// Run `frames` frames with the player at `at` (pressing OK on `ok_frame`), answering everything; stop at a map jump.
fn run(rt: &mut FieldRuntime, frames: u32, at: Option<(i32, i32, i32)>, ok_frame: Option<u32>) -> Outcome {
    run_path(rt, frames, &[], at, ok_frame)
}

/// [`run`], walking through `path` first (one point per frame) and then standing at `at`.
fn run_path(rt: &mut FieldRuntime, frames: u32, path: &[(i32, i32, i32)], at: Option<(i32, i32, i32)>, ok_frame: Option<u32>) -> Outcome {
    run_facing(rt, frames, path, at, ok_frame, 0)
}

fn run_facing(rt: &mut FieldRuntime, frames: u32, path: &[(i32, i32, i32)], at: Option<(i32, i32, i32)>, ok_frame: Option<u32>, dir: u8) -> Outcome {
    let mut out = Outcome::default();
    for f in 0..frames {
        let at = path.get(f as usize).copied().or(at);
        let player = at.map(|(x, y, z)| PlayerInput { x, y, z, ok: ok_frame == Some(f), keys_pressed: if ok_frame == Some(f) { 0x20 } else { 0 }, keys_held: if ok_frame == Some(f) { 0x20 } else { 0 }, dir, ..Default::default() });
        rt.tick(player);
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
                Event::Command(Command::MapJump { field, x, y, triangle, dir }) | Event::Command(Command::Minigame { field, x, y, triangle, dir, .. }) => {
                    out.jump = Some((field, x, y, triangle, dir));
                    return out;
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
                Event::Command(c) if c.returns_to_field() => rt.complete_command(),
                Event::Command(Command::DiscChange { .. }) => rt.complete_command(),
                Event::Command(Command::GameOver) => return out,
                _ => {}
            }
        }
    }
    out
}

fn open(lgp: &mut LgpArchive<std::fs::File>, name: &str) -> Option<(FieldFile, Arc<fmv_ff7::field::Walkmesh>)> {
    let field = load_field(lgp, name).ok()?;
    let walk = Arc::new(field.walkmesh().ok()?);
    Some((field, walk))
}

fn main() {
    let Some(dir) = data_dir() else {
        eprintln!("FMV_FF7_DATA is not set; skipping");
        return;
    };
    let mut args = std::env::args().skip(1);
    let (mut field_name, mut steps, mut start_moment) = ("md1stin".to_string(), 60u32, None::<u16>);
    while let Some(a) = args.next() {
        let v = args.next().unwrap_or_default();
        match a.as_str() {
            "--from" => field_name = v,
            "--steps" => steps = v.parse().unwrap_or(steps),
            "--moment" => start_moment = v.parse().ok(),
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
    let mut arrival: Option<(i32, i32, i32)> = None;
    let mut visited = Vec::new();
    let mut seen: Vec<(String, u16)> = Vec::new();
    // Untried gateway choices: (field, banks, arrival, seen, step label), most recent last.
    let mut alternatives: Vec<(String, Banks, Option<(i32, i32, i32)>, Vec<(String, u16)>, String)> = Vec::new();
    let mut best_moment = 0u16;
    let mut actions: Vec<(String, u16)> = Vec::new();
    let mut images: std::collections::HashSet<(String, u64)> = std::collections::HashSet::new();
    let mut entered: std::collections::HashSet<(String, u64)> = std::collections::HashSet::new();
    for step in 0..steps {
        // A field already entered with this very story leads where it led before: take the next alternative.
        if !entered.insert((field_name.clone(), banks_key(&banks))) {
            match alternatives.pop() {
                Some((f, b, a, sn, _)) => {
                    field_name = f;
                    banks = b;
                    arrival = a;
                    seen = sn;
                    continue;
                }
                None => {
                    println!("{step:>3} nothing left to try (best {best_moment})");
                    break;
                }
            }
        }
        let Some((field, walk)) = open(&mut lgp, &field_name) else {
            println!("STOP cannot load {field_name}");
            break;
        };
        let section = field.section(fmv_ff7::field::section::SCRIPT).expect("script");
        let mut rt = FieldRuntime::new(section, banks.clone()).expect("runtime");
        rt.set_walkmesh(Arc::clone(&walk));
        rt.init();
        let at = arrival.or_else(|| rt.pc_model().map(|m| (m.x, m.y, m.z)));
        let before = moment(&rt);
        best_moment = best_moment.max(before);
        images.insert((field_name.clone(), story_key(&rt)));
        let settle = run(&mut rt, SETTLE_FRAMES, at, None);
        visited.push(field_name.clone());
        seen.push((field_name.clone(), before));
        if let Some(jump) = settle.jump {
            let to = maplist.name(jump.0).unwrap_or("?").to_string();
            println!("{step:>3} {field_name:<10} moment {before}->{} on its own: battles {:?} movies {:?} windows {} -> {to}", moment(&rt), settle.battles, settle.movies, settle.windows);
            banks = rt.mem.banks.clone();
            field_name = to;
            arrival = Some((i32::from(jump.1), i32::from(jump.2), 0));
            continue;
        }
        // Try every triangle, standing and with OK.
        let mut best: Option<(i32, usize, bool, FieldRuntime, Outcome)> = None;
        // Every scored action, as a state to come back to: (score, field it leaves the player in, story, arrival).
        let mut cands: Vec<(i32, String, Banks, Option<(i32, i32, i32)>, String)> = Vec::new();
        for t in 0..walk.triangles.len() {
            let v = walk.triangles[t];
            let c = ((i32::from(v[0].x) + i32::from(v[1].x) + i32::from(v[2].x)) / 3, (i32::from(v[0].y) + i32::from(v[1].y) + i32::from(v[2].y)) / 3, (i32::from(v[0].z) + i32::from(v[1].z) + i32::from(v[2].z)) / 3);
            for ok in [false, true] {
                let mut probe = rt.clone();
                let mut out = run(&mut probe, PROBE_FRAMES, Some(c), ok.then_some(2));
                let changed = story_changed(&probe, &rt);
                if (changed || out.windows > 0) && out.jump.is_none() {
                    // The scene it started may end in a map jump: let it play on.
                    let more = run(&mut probe, 3600, Some(c), None);
                    out.jump = more.jump;
                    out.battles.extend(more.battles);
                    out.movies.extend(more.movies);
                    out.windows += more.windows;
                }
                let moved = moment(&probe) > moment(&rt);
                // A new field beats a flag (a lift's ride back is a flag that undoes the ride down).
                let score = match (out.jump.is_some(), moved, story_changed(&probe, &rt)) {
                    (true, true, _) => 5,
                    (false, true, _) => 4,
                    (true, false, _) => 3,
                    (false, false, true) => 2,
                    _ => 0,
                };
                // A jump back to a field already visited at this moment is a step back.
                let back = out.jump.is_some_and(|j| visited.iter().any(|v| Some(v.as_str()) == maplist.name(j.0)) && !moved);
                let _ = back;
                let landed = out.jump.and_then(|j| maplist.name(j.0)).map_or_else(|| field_name.clone(), str::to_string);
                let score = if images.contains(&(landed, story_key(&probe))) { 0 } else { score };
                if score > 0 {
                    let land = out.jump.and_then(|j| maplist.name(j.0)).map_or_else(|| field_name.clone(), str::to_string);
                    let arr = out.jump.map(|j| (i32::from(j.1), i32::from(j.2), 0)).or(arrival);
                    cands.push((score, land.clone(), probe.mem.banks.clone(), arr, format!("{field_name} -> {land} (score {score}, moment {})", moment(&probe))));
                }
                if score > 0 && best.as_ref().is_none_or(|b| score > b.0) {
                    best = Some((score, t, ok, probe, out));
                }
            }
        }
        // Talk: stand beside each talkable model, facing it, and press OK.
        let pc = rt.state.pc_model;
        let talkable: Vec<(usize, i32, i32, i32)> = rt.models().iter().enumerate().filter(|(i, m)| Some(*i) != pc && !m.talk_off).map(|(i, m)| (i, m.x, m.y, m.z)).collect();
        for (mi, mx, my, mz) in talkable {
            for (ox, oy) in [(-40, 0), (40, 0), (0, -40), (0, 40)] {
                let at = (mx + ox, my + oy, mz);
                let dir = fmv_ff7::runtime::direction_of(i64::from(-ox), i64::from(-oy));
                let mut probe = rt.clone();
                let mut out = run_facing(&mut probe, PROBE_FRAMES, &[], Some(at), Some(2), dir);
                if (story_changed(&probe, &rt) || out.windows > 0) && out.jump.is_none() {
                    let more = run_facing(&mut probe, 3600, &[], Some(at), None, dir);
                    out.jump = more.jump;
                    out.battles.extend(more.battles);
                    out.movies.extend(more.movies);
                    out.windows += more.windows;
                }
                let moved = moment(&probe) > moment(&rt);
                let changed = story_changed(&probe, &rt);
                let score = match (out.jump.is_some(), moved, changed) { (true, true, _) => 5, (false, true, _) => 4, (true, false, _) => 3, (false, false, true) => 2, _ => 0 };
                let back = out.jump.is_some_and(|j| visited.iter().any(|v| Some(v.as_str()) == maplist.name(j.0)) && !moved);
                let _ = back;
                let landed = out.jump.and_then(|j| maplist.name(j.0)).map_or_else(|| field_name.clone(), str::to_string);
                let score = if images.contains(&(landed, story_key(&probe))) { 0 } else { score };
                if score > 0 {
                    let land = out.jump.and_then(|j| maplist.name(j.0)).map_or_else(|| field_name.clone(), str::to_string);
                    let arr = out.jump.map(|j| (i32::from(j.1), i32::from(j.2), 0)).or(arrival);
                    cands.push((score, land.clone(), probe.mem.banks.clone(), arr, format!("{field_name} -> {land} (score {score}, moment {})", moment(&probe))));
                }
                if score > 0 && best.as_ref().is_none_or(|b| score > b.0) {
                    println!("    talk to model {mi} ({}) scores {score}", rt.entity_name(rt.models()[mi].entity).unwrap_or(""));
                    best = Some((score, usize::MAX / 2 - mi, true, probe, out));
                }
                if score > 0 {
                    break;
                }
            }
        }
        // Lines: walk across each, and stand on its middle pressing OK.
        let lines: Vec<_> = rt.lines().iter().filter(|l| l.active).map(|l| (l.a, l.b)).collect();
        for (i, (a, b)) in lines.iter().enumerate() {
            let (mx, my, mz) = ((i32::from(a.0) + i32::from(b.0)) / 2, (i32::from(a.1) + i32::from(b.1)) / 2, (i32::from(a.2) + i32::from(b.2)) / 2);
            let (dx, dy) = (f64::from(b.0 - a.0), f64::from(b.1 - a.1));
            let n = (dx * dx + dy * dy).sqrt().max(1.0);
            let (nx, ny) = (-dy / n, dx / n);
            let walk: Vec<(i32, i32, i32)> = (-6..=6).map(|k| (mx + (nx * 5.0 * f64::from(k)) as i32, my + (ny * 5.0 * f64::from(k)) as i32, mz)).collect();
            for (mode, path, ok) in [("across", walk.clone(), None), ("back across", walk.iter().rev().copied().collect(), None), ("on +ok", vec![], Some(2))] {
                let mut probe = rt.clone();
                let mut out = run_path(&mut probe, PROBE_FRAMES, &path, Some(*path.last().unwrap_or(&(mx, my, mz))), ok);
                if (story_changed(&probe, &rt) || out.windows > 0) && out.jump.is_none() {
                    let more = run(&mut probe, 3600, Some((mx, my, mz)), None);
                    out.jump = more.jump;
                    out.battles.extend(more.battles);
                    out.movies.extend(more.movies);
                    out.windows += more.windows;
                }
                let moved = moment(&probe) > moment(&rt);
                let score = match (out.jump.is_some(), moved, story_changed(&probe, &rt)) { (true, true, _) => 5, (false, true, _) => 4, (true, false, _) => 3, (false, false, true) => 2, _ => 0 };
                let back = out.jump.is_some_and(|j| visited.iter().any(|v| Some(v.as_str()) == maplist.name(j.0)) && !moved);
                let _ = back;
                let landed = out.jump.and_then(|j| maplist.name(j.0)).map_or_else(|| field_name.clone(), str::to_string);
                let score = if images.contains(&(landed, story_key(&probe))) { 0 } else { score };
                if score > 0 {
                    let land = out.jump.and_then(|j| maplist.name(j.0)).map_or_else(|| field_name.clone(), str::to_string);
                    let arr = out.jump.map(|j| (i32::from(j.1), i32::from(j.2), 0)).or(arrival);
                    cands.push((score, land.clone(), probe.mem.banks.clone(), arr, format!("{field_name} -> {land} (score {score}, moment {})", moment(&probe))));
                }
                if score > 0 && best.as_ref().is_none_or(|b| score > b.0) {
                    println!("    line {i} {mode} scores {score}");
                    best = Some((score, usize::MAX - i, mode == "on +ok", probe, out));
                }
            }
        }
        let here = (field_name.clone(), moment(&rt));
        let done_here = actions.iter().filter(|a| **a == here).count();
        if done_here >= 6 {
            best = None;
        }
        // The runners-up wait as alternatives (lowest score pushed first, so the best of them comes back first).
        if let Some(b) = &best {
            let chosen = (b.0, story_key(&b.3));
            cands.sort_by_key(|c| c.0);
            for c in cands {
                if (c.0, banks_key(&c.2)) == chosen {
                    continue;
                }
                let mut sn = seen.clone();
                sn.push((field_name.clone(), moment(&rt)));
                alternatives.push((c.1, c.2, c.3, sn, c.4));
            }
        }
        let Some((_, t, ok, probe, out)) = best else {
            // No script moves things on: walk through a gateway of the field's data into a field not yet seen at this
            // moment (the host's gateways, not a script's map jump).
            let m = moment(&rt);
            // Every gateway into a field not seen at this moment is a choice; the others wait as alternatives (a field never
            // visited first, since the story mostly leads forward).
            let mut choices: Vec<fmv_ff7::field::Gateway> = field.triggers().ok().map(|tr| {
                tr.gateways.iter().filter(|g| g.target_field_id != fmv_ff7::field::Gateway::UNUSED_FIELD_ID)
                    .filter(|g| maplist.name(g.target_field_id).is_some_and(|n| !seen.contains(&(n.to_string(), m))))
                    .cloned().collect()
            }).unwrap_or_default();
            choices.sort_by_key(|g| maplist.name(g.target_field_id).is_some_and(|n| visited.iter().any(|v| v == n)));
            let mut choices = choices.into_iter();
            let Some(g) = choices.next() else {
                println!("{step:>3} {field_name:<10} STUCK at moment {m} (best so far {best_moment})");
                match alternatives.pop() {
                    Some((f, b, a, sn, label)) => {
                        println!("    back to {label}");
                        field_name = f;
                        banks = b;
                        arrival = a;
                        seen = sn;
                        continue;
                    }
                    None => break,
                }
            };
            for alt in choices.rev() {
                let name = maplist.name(alt.target_field_id).unwrap_or("?").to_string();
                let mut sn = seen.clone();
                sn.push((field_name.clone(), m));
                alternatives.push((name.clone(), rt.mem.banks.clone(), Some((i32::from(alt.destination[0]), i32::from(alt.destination[1]), 0)), sn, format!("{field_name} gateway -> {name} (moment {m})")));
            }
            let to = maplist.name(g.target_field_id).unwrap_or("?").to_string();
            println!("{step:>3} {field_name:<10} gateway -> {to} (moment {m})");
            seen.push((field_name.clone(), m));
            banks = rt.mem.banks.clone();
            field_name = to;
            arrival = Some((i32::from(g.destination[0]), i32::from(g.destination[1]), 0));
            continue;
        };
        actions.push(here.clone());
        let after = moment(&probe);
        let to = out.jump.and_then(|j| maplist.name(j.0)).unwrap_or("-");
        let what = if t > usize::MAX / 2 + 1000 { format!("line {}", usize::MAX - t) } else if t > walk.triangles.len() { format!("talk to model {}", usize::MAX / 2 - t) } else { format!("tri {t}") };
        println!("{step:>3} {field_name:<10} {what}{} moment {}->{after}: battles {:?} movies {:?} windows {} -> {to}", if ok { " +ok" } else { "" }, moment(&rt), out.battles, out.movies, out.windows);
        banks = probe.mem.banks.clone();
        if let Some(jump) = out.jump {
            field_name = maplist.name(jump.0).unwrap_or("?").to_string();
            arrival = Some((i32::from(jump.1), i32::from(jump.2), 0));
        } else {
            // Same field: run on from the probe's state in the next step (re-entering keeps the story, not the field's scratch).
            arrival = if t < walk.triangles.len() { Some((c_of(&walk, t).0, c_of(&walk, t).1, 0)) } else { arrival };
        }
    }
}

fn c_of(walk: &fmv_ff7::field::Walkmesh, t: usize) -> (i32, i32) {
    let v = walk.triangles[t];
    ((i32::from(v[0].x) + i32::from(v[1].x) + i32::from(v[2].x)) / 3, (i32::from(v[0].y) + i32::from(v[1].y) + i32::from(v[2].y)) / 3)
}
