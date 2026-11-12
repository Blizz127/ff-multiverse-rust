//! Run one owned field in the live runtime and show what every entity is doing (needs the owner's data).
//!
//! ```text
//! FMV_FF7_DATA=<.../data> cargo run -p fmv-ff7 --example runtime_trace -- FIELD [--frames N] [--every N] [--at X,Y] [--ok-every N] [--moment M]
//! ```
//!
//! The field starts from the fresh reset story (every bit known; `--moment` sets the story moment), runs `N` frames
//! (default 600) with the player standing at `--at` (or nowhere), dismisses windows (first answer), completes
//! battles, menus and movies at once, and prints the events and, every `--every` frames, each entity's priority, PC
//! and the instruction it is on. Mnemonics, numbers and entity names only: no game text.
#[path = "../tests/script_common.rs"]
mod common;

use common::{data_dir, flevel_path, load_field};
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::runtime::{Banks, Event, FieldRuntime, PlayerInput};
use fmv_ff7::script::decode_at;
use fmv_ff7::story::StoryState;

fn main() {
    let Some(dir) = data_dir() else {
        eprintln!("FMV_FF7_DATA is not set; skipping");
        return;
    };
    let mut args = std::env::args().skip(1);
    let Some(name) = args.next() else {
        eprintln!("usage: runtime_trace FIELD [--frames N] [--every N] [--at X,Y] [--ok-every N] [--moment M]");
        return;
    };
    let (mut frames, mut every, mut at, mut ok_every, mut moment) = (600u64, 150u64, None, 0u64, None);
    let (mut after, mut then_tri) = (0u64, None::<usize>);
    let (mut banks_file, mut walk_tri) = (None::<String>, None::<usize>);
    let mut sets: Vec<(u8, u8, u8)> = Vec::new();
    while let Some(a) = args.next() {
        let v = args.next().unwrap_or_default();
        match a.as_str() {
            "--frames" => frames = v.parse().unwrap_or(frames),
            "--every" => every = v.parse().unwrap_or(every),
            "--ok-every" => ok_every = v.parse().unwrap_or(0),
            "--moment" => moment = v.parse::<u16>().ok(),
            "--at" => at = v.split_once(',').and_then(|(x, y)| Some((x.parse::<i32>().ok()?, y.parse::<i32>().ok()?))),
            "--after" => after = v.parse().unwrap_or(0),
            "--banks" => banks_file = Some(v),
            "--walk-tri" => walk_tri = v.parse::<usize>().ok(),
            "--set" => {
                if let Some((sa, val)) = v.split_once('=') {
                    if let (Some((sel, addr)), Ok(val)) = (sa.split_once(':'), val.parse::<u8>()) {
                        if let (Ok(sel), Ok(addr)) = (sel.parse::<u8>(), addr.parse::<u8>()) {
                            sets.push((sel, addr, val));
                        }
                    }
                }
            }
            "--then-tri" => then_tri = v.parse::<usize>().ok(),
            _ => {}
        }
    }
    let mut lgp = LgpArchive::open_path(&flevel_path(&dir)).expect("flevel");
    let field = load_field(&mut lgp, &name).expect("field");
    let section = field.section(fmv_ff7::field::section::SCRIPT).expect("script");
    let mut banks = Banks::from_story(&StoryState::fresh_reset()).expect("complete");
    if let Some(b) = banks_file.as_deref().and_then(|f| std::fs::read_to_string(f).ok()).and_then(|h| Banks::from_hex(h.trim())) {
        banks = b;
    }
    if let Some(m) = moment {
        let [lo, hi] = m.to_le_bytes();
        banks.set(0, lo);
        banks.set(1, hi);
    }
    for (sel, addr, val) in &sets {
        if let Some(base) = fmv_ff7::runtime::memory::base_of(*sel) {
            banks.set(base + usize::from(*addr), *val);
        }
    }
    let mut rt = FieldRuntime::new(section, banks).expect("runtime");
    if let Ok(w) = field.walkmesh() {
        rt.set_walkmesh(std::sync::Arc::new(w));
    }
    rt.init();
    let dump = |rt: &FieldRuntime, label: &str| {
        println!("-- {label}: frame {} moment {} party {:?} lock {}", rt.frame(), u16::from_le_bytes([rt.mem.banks.get(0), rt.mem.banks.get(1)]), rt.party(), rt.state.character_lock);
        for e in 0..rt.entity_count() {
            let Some((prio, pc)) = rt.entity_state(e) else { continue };
            let op = decode_at(rt_code(section), pc, 0).map(|i| i.to_string()).unwrap_or_else(|e| format!("? {e}"));
            println!("   {:>2} {:<8} p{} {}", e, rt.entity_name(e).unwrap_or(""), prio, op);
        }
        for (i, m) in rt.models().iter().enumerate() {
            println!("   model {i} entity {} at ({},{},{}) tri {} dir {} vis {} talk_off {}", m.entity, m.x, m.y, m.z, m.triangle, m.dir, m.visible, m.talk_off);
        }
    };
    dump(&rt, "after init");
    let walk = field.walkmesh().ok();
    let tri_centre = |t: usize| walk.as_ref().and_then(|w| w.triangles.get(t)).map(|v| ((i32::from(v[0].x) + i32::from(v[1].x) + i32::from(v[2].x)) / 3, (i32::from(v[0].y) + i32::from(v[1].y) + i32::from(v[2].y)) / 3));
    for f in 1..=frames {
        if f == after.max(1) {
            if let Some(c) = then_tri.and_then(tri_centre) {
                println!("frame {f}: player moves to the centre of triangle {:?} at {c:?}", then_tri);
                at = Some(c);
            }
        }
        // while the scripts hold or move the player, the player is where they put it
        if rt.player_scripted() {
            if let Some(m) = rt.player_model() {
                at = Some((m.x, m.y));
            }
        }
        if let (true, Some(target)) = (f > after.max(1), walk_tri.and_then(tri_centre)) {
            // follow the scripts while they hold the player, else walk 24 units a frame toward the triangle's centre
            let cur = if rt.player_scripted() { rt.player_model().map(|m| (m.x, m.y)) } else { at }.or_else(|| rt.player_model().map(|m| (m.x, m.y)));
            if let Some((x, y)) = cur {
                let (dx, dy) = (f64::from(target.0 - x), f64::from(target.1 - y));
                let d = (dx * dx + dy * dy).sqrt();
                at = Some(if rt.player_scripted() || d <= 24.0 { if rt.player_scripted() { (x, y) } else { target } } else { (x + (dx / d * 24.0) as i32, y + (dy / d * 24.0) as i32) });
            }
        }
        let z = rt.player_model().map_or(0, |m| m.z);
        let player = at.map(|(x, y)| PlayerInput { x, y, z, ok: ok_every > 0 && f % ok_every == 0, dir: 64, ..Default::default() });
        rt.tick(player);
        for ev in rt.drain_events() {
            match &ev {
                Event::Window { window, dialog, ask, entity, .. } => {
                    println!("frame {f}: WINDOW {window} dialog {dialog} ask {ask:?} by {}", rt.entity_name(*entity).unwrap_or(""));
                    rt.close_window(*window);
                    rt.answer(*window, 0);
                }
                Event::Command(c) => {
                    println!("frame {f}: COMMAND {c:?}");
                    if c.returns_to_field() {
                        rt.complete_command();
                    }
                }
                Event::Presentation { .. } => {}
                Event::Control { .. } => println!("frame {f}: {ev:?} fade {:?}", rt.stage_fade_color()),
                other => println!("frame {f}: {other:?}"),
            }
        }
        if rt.command().is_some_and(|c| !c.returns_to_field()) {
            dump(&rt, "left the field");
            break;
        }
        if every > 0 && f % every == 0 {
            dump(&rt, "running");
        }
    }
    let mut u: Vec<_> = rt.coverage().unsupported.iter().collect();
    u.sort_by(|a, b| b.1.cmp(a.1));
    println!("unsupported executed: {u:?}");
}

fn rt_code(section: &[u8]) -> &[u8] {
    section
}
