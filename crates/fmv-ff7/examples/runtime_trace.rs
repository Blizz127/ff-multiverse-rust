//! Run one owned field in the live runtime and show what every entity is doing (needs the owner's data).
//!
//! ```text
//! FMV_FF7_DATA=<.../data> cargo run -p fmv-ff7 --example runtime_trace -- FIELD [--frames N] [--every N] [--at X,Y] [--ok-every N] [--moment M] \
//!     [--choice DIALOG=INDEX]... [--answer-delay-frames N] [--walk-tri N] [--speed UNITS_PER_SECOND]
//! ```
//!
//! The field starts from the fresh reset story (every bit known; `--moment` sets the story moment), runs `N` frames
//! (default 600) with the player standing at `--at` (or nowhere), dismisses windows (first answer), completes
//! battles, menus and movies at once, and prints the events and, every `--every` frames, each entity's priority, PC
//! and the instruction it is on. Choices default to zero and are relative to the question's first line. Answer
//! delay defaults to zero; field frames continue normally while waiting. A close or reopen cancels the old answer.
//! Walking defaults to 200 units/sec (formerly 720); input coordinates are rounded to whole field units. --then-tri
//! remains a diagnostic placement, not gameplay evidence. Mnemonics, numbers and entity names only: no game text.
#[path = "../tests/script_common.rs"]
mod common;

use common::{data_dir, flevel_path, load_field};
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::runtime::{Banks, Event, FieldRuntime, PlayerInput};
use fmv_ff7::script::decode_at;
use fmv_ff7::story::StoryState;
use std::collections::BTreeMap;

#[derive(Debug, Default)]
struct Choices(BTreeMap<u16, u8>);

impl Choices {
    fn insert(&mut self, value: &str) -> Result<(), String> {
        let invalid = || format!("invalid --choice {value:?}; expected decimal DIALOG=INDEX (dialog 0..65535, index 0..255)");
        let (dialog, index) = value.split_once('=').ok_or_else(invalid)?;
        if dialog.is_empty() || index.is_empty() || !dialog.bytes().all(|b| b.is_ascii_digit()) || !index.bytes().all(|b| b.is_ascii_digit()) {
            return Err(invalid());
        }
        let dialog = dialog.parse::<u16>().map_err(|_| invalid())?;
        let index = index.parse::<u8>().map_err(|_| invalid())?;
        if self.0.contains_key(&dialog) { return Err(format!("duplicate --choice for dialog {dialog}")); }
        self.0.insert(dialog, index);
        Ok(())
    }

    fn answer(&self, dialog: u8, (first, last): (u8, u8)) -> Result<u8, String> {
        let index = self.0.get(&u16::from(dialog)).copied().unwrap_or(0);
        if first > last || index > last - first {
            return Err(format!("choice {dialog}={index} is out of range for ASK lines {first}..{last}; indexes are zero-based"));
        }
        Ok(index)
    }
}

#[derive(Debug, Clone)]
struct PendingAnswer {
    entity: usize,
    dialog: u8,
    ask: Option<(u8, u8)>,
    choice: Option<u8>,
    due: u64,
}

struct Answers {
    choices: Choices,
    delay: u64,
    pending: BTreeMap<u8, PendingAnswer>,
}

impl Answers {
    fn new(choices: Choices, delay: u64) -> Self { Self { choices, delay, pending: BTreeMap::new() } }

    fn observe(&mut self, event: &Event, frame: u64) -> Result<(), String> {
        match *event {
            Event::Window { window, entity, dialog, ask, .. } => {
                // A new event starts a new lifetime even when every visible identity field is unchanged.
                self.pending.remove(&window);
                let choice = ask.map(|lines| self.choices.answer(dialog, lines)).transpose()?;
                let due = frame.checked_add(self.delay).ok_or("answer delay overflows frame counter")?;
                self.pending.insert(window, PendingAnswer { entity, dialog, ask, choice, due });
            }
            Event::WindowDone { window } | Event::WindowClosedByScript { window } => { self.pending.remove(&window); }
            _ => {}
        }
        Ok(())
    }

    fn deliver(&mut self, rt: &mut FieldRuntime, frame: u64) -> Result<Vec<(u8, u8, Option<u8>)>, String> {
        let mut sent = Vec::new();
        for window in self.pending.keys().copied().collect::<Vec<_>>() {
            let pending = &self.pending[&window];
            let current = rt.windows().get(usize::from(window)).and_then(|w| w.open.as_ref());
            let matches = rt.window_ready(window) && current.is_some_and(|open| !open.closed && open.entity == pending.entity && open.dialog == pending.dialog
                && open.ask.map(|ask| (ask.first, ask.last)) == pending.ask);
            if !matches { self.pending.remove(&window); continue; }
            if frame < pending.due { continue; }
            let (dialog, choice) = (pending.dialog, pending.choice);
            if let Some(choice) = choice {
                if !rt.answer(window, choice) { return Err(format!("runtime refused choice {dialog}={choice} in window {window}")); }
            } else {
                // Acceptance starts closing; it does not mean the owning script has finished shrinking it.
                if !rt.close_window(window) { return Err(format!("runtime refused dismissal of dialog {dialog} in window {window}")); }
            }
            self.pending.remove(&window);
            sent.push((window, dialog, choice));
        }
        Ok(sent)
    }
}

fn fatal(message: String) -> ! {
    eprintln!("runtime_trace: {message}");
    std::process::exit(2)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(name) = args.next() else {
        eprintln!("usage: runtime_trace FIELD [--frames N] [--every N] [--at X,Y] [--ok-every N] [--moment M]");
        return;
    };
    let (mut frames, mut every, mut at, mut ok_every, mut moment) = (600u64, 150u64, None, 0u64, None);
    let (mut after, mut then_tri) = (0u64, None::<usize>);
    let (mut banks_file, mut walk_tri) = (None::<String>, None::<usize>);
    let (mut choices, mut answer_delay, mut speed) = (Choices::default(), 0u64, 200.0f64);
    let mut sets: Vec<(u8, u8, u8)> = Vec::new();
    while let Some(a) = args.next() {
        let v = args.next().unwrap_or_default();
        match a.as_str() {
            "--frames" => frames = v.parse().unwrap_or(frames),
            "--every" => every = v.parse().unwrap_or(every),
            "--ok-every" => ok_every = v.parse().unwrap_or(0),
            "--moment" => moment = v.parse::<u16>().ok(),
            "--at" => {
                at = v.split_once(',').and_then(|(x, y)| Some((x.parse::<f64>().ok()?, y.parse::<f64>().ok()?))).filter(|(x, y)| x.is_finite() && y.is_finite());
                if at.is_none() { fatal(format!("invalid --at {v:?}; expected finite X,Y")); }
            }
            "--after" => after = v.parse().unwrap_or(0),
            "--banks" => banks_file = Some(v),
            "--walk-tri" => walk_tri = v.parse::<usize>().ok(),
            "--choice" => choices.insert(&v).unwrap_or_else(|error| fatal(error)),
            "--answer-delay-frames" => {
                if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) { fatal(format!("invalid --answer-delay-frames {v:?}")); }
                answer_delay = v.parse().unwrap_or_else(|_| fatal(format!("invalid --answer-delay-frames {v:?}")));
            }
            "--speed" => {
                speed = v.parse::<f64>().ok().filter(|s| s.is_finite() && *s > 0.0).unwrap_or_else(|| fatal(format!("invalid --speed {v:?}; expected positive finite units/sec")));
            }
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
    let Some(dir) = data_dir() else {
        eprintln!("FMV_FF7_DATA is not set; skipping");
        return;
    };
    let mut answers = Answers::new(choices, answer_delay);
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
    // Match the live server's animation timing: fallback lengths can hide script-ordering bugs.
    if let (Ok(mut chars), Ok(loader)) = (LgpArchive::open_path(dir.join("field/char.lgp")), field.model_loader()) {
        rt.set_animation_frame_counts(&fmv_ff7::runtime::AnimState::frame_counts_from(&mut chars, &loader));
    }
    rt.init();
    let dump = |rt: &FieldRuntime, label: &str| {
        println!("-- {label}: frame {} moment {} party {:?} lock {} scratch5:7={} scratch5:8={}", rt.frame(), u16::from_le_bytes([rt.mem.banks.get(0), rt.mem.banks.get(1)]), rt.party(), rt.state.character_lock, rt.mem.scratch[7], rt.mem.scratch[8]);
        for e in 0..rt.entity_count() {
            let Some((prio, pc)) = rt.entity_state(e) else { continue };
            let op = decode_at(rt_code(section), pc, 0).map(|i| i.to_string()).unwrap_or_else(|e| format!("? {e}"));
            println!("   {:>2} {:<8} p{} broken={} {}", e, rt.entity_name(e).unwrap_or(""), prio, rt.entity_broken(e), op);
        }
        for (i, m) in rt.models().iter().enumerate() {
            println!("   model {i} entity {} at ({},{},{}) tri {} dir {} vis {} talk_off {}", m.entity, m.x, m.y, m.z, m.triangle, m.dir, m.visible, m.talk_off);
            println!("      animation {:?}", rt.model_animation(i));
        }
    };
    println!("trace input at={at:?} speed={speed} answer_delay_frames={answer_delay} choices={:?}; input is diagnostic, not a TCP replay", answers.choices.0);
    dump(&rt, "after init");
    let walk = field.walkmesh().ok();
    let tri_centre = |t: usize| walk.as_ref().and_then(|w| w.triangles.get(t)).map(|v| (f64::from((i32::from(v[0].x) + i32::from(v[1].x) + i32::from(v[2].x)) / 3), f64::from((i32::from(v[0].y) + i32::from(v[1].y) + i32::from(v[2].y)) / 3)));
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
                at = Some((f64::from(m.x), f64::from(m.y)));
            }
        }
        if let (true, Some(target)) = (f > after.max(1), walk_tri.and_then(tri_centre)) {
            // Retain fractional movement between field frames; only the supplied PlayerInput is rounded.
            let cur = if rt.player_scripted() { rt.player_model().map(|m| (f64::from(m.x), f64::from(m.y))) } else { at }.or_else(|| rt.player_model().map(|m| (f64::from(m.x), f64::from(m.y))));
            if let Some((x, y)) = cur {
                let (dx, dy) = (target.0 - x, target.1 - y);
                let d = (dx * dx + dy * dy).sqrt();
                let stride = speed / 30.0;
                at = Some(if rt.player_scripted() || d <= stride { if rt.player_scripted() { (x, y) } else { target } } else { (x + dx / d * stride, y + dy / d * stride) });
            }
        }
        let z = rt.player_model().map_or(0, |m| m.z);
        let player = at.map(|(x, y)| PlayerInput { x: x.round() as i32, y: y.round() as i32, z, ok: ok_every > 0 && f % ok_every == 0, dir: 64, ..Default::default() });
        rt.tick(player);
        for ev in rt.drain_events() {
            answers.observe(&ev, f).unwrap_or_else(|error| fatal(error));
            match &ev {
                Event::Window { window, dialog, ask, entity, .. } => {
                    println!("frame {f}: WINDOW {window} dialog {dialog} ask {ask:?} by {}", rt.entity_name(*entity).unwrap_or(""));
                    println!("frame {f}: answer queued for frame {}", answers.pending[window].due);
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
        for (window, dialog, choice) in answers.deliver(&mut rt, f).unwrap_or_else(|error| fatal(error)) {
            println!("frame {f}: ANSWER window={window} dialog={dialog} choice={choice:?}");
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

#[cfg(test)]
mod tests {
    use super::*;

    fn window(window: u8, entity: usize, dialog: u8) -> Event {
        Event::Window { window, entity, dialog, ask: Some((1, 3)), offset: 0 }
    }

    /// A director starts a question; an independent clock sets a flag after twenty normal field frames.
    fn runtime() -> FieldRuntime {
        let code_start = 0x20 + 3 * (8 + 64);
        let mut code = Vec::new();
        let mut entries = [[0u16; 32]; 3];
        for (entity, row) in entries.iter_mut().enumerate() {
            for (slot, entry) in row.iter_mut().enumerate() {
                *entry = (code_start + code.len()) as u16;
                match (entity, slot) {
                    (0, 0) => code.extend_from_slice(&[0, 3, 1, 0xC1, 0]),
                    (1, 0) => code.extend_from_slice(&[0, 0]),
                    (1, 1) => code.extend_from_slice(&[0x48, 0x05, 0, 0, 1, 3, 6, 0]),
                    (2, 0) => code.extend_from_slice(&[0, 0x24, 20, 0, 0x80, 0x10, 0x40, 1, 0]),
                    _ => code.push(0),
                }
            }
        }
        let mut bytes = vec![0; 0x20];
        bytes[0..3].copy_from_slice(&[2, 5, 3]);
        bytes[4..6].copy_from_slice(&((code_start + code.len()) as u16).to_le_bytes());
        bytes[8..10].copy_from_slice(&512u16.to_le_bytes());
        bytes.extend_from_slice(b"directorworker\0\0clock\0\0\0");
        for row in entries { for entry in row { bytes.extend_from_slice(&entry.to_le_bytes()); } }
        bytes.extend(code);
        bytes.extend_from_slice(&[1, 0, 4, 0, 0xFF]);
        let mut rt = FieldRuntime::new(&bytes, Banks::default()).unwrap();
        rt.init();
        rt
    }

    #[test]
    fn delayed_answers_advance_the_field_and_keep_zero_based_choices() {
        for delay in [0, 30] {
            let mut choices = Choices::default();
            choices.insert("0=2").unwrap();
            let mut answers = Answers::new(choices, delay);
            let mut rt = runtime();
            let (mut opened, mut sent) = (None, Vec::new());
            for frame in 1..=40 {
                rt.tick(None);
                for event in rt.drain_events() {
                    if matches!(event, Event::Window { .. }) { opened = Some(frame); }
                    answers.observe(&event, frame).unwrap();
                }
                for response in answers.deliver(&mut rt, frame).unwrap() { sent.push((frame, response)); }
                if frame == 25 {
                    assert_eq!(rt.mem.banks.get(0x40), 1, "other scripts advance while the answer waits");
                    assert_eq!(rt.window_waiting(), delay == 30);
                }
            }
            assert_eq!(sent, [(opened.unwrap() + delay, (0, 0, Some(2)))]);
            assert_eq!(rt.mem.scratch[6], 2);
        }
    }

    #[test]
    fn closing_or_reopening_cancels_the_previous_window_deadline() {
        let mut answers = Answers::new(Choices::default(), 30);
        answers.observe(&window(0, 1, 0), 1).unwrap();
        answers.observe(&Event::WindowClosedByScript { window: 0 }, 5).unwrap();
        assert!(answers.pending.is_empty());
        answers.observe(&window(0, 1, 0), 7).unwrap();
        assert_eq!(answers.pending[&0].due, 37);
        answers.observe(&window(0, 1, 0), 10).unwrap();
        assert_eq!(answers.pending[&0].due, 40, "identical reopening starts a new lifetime");
        answers.observe(&window(1, 2, 1), 15).unwrap();
        assert_eq!(answers.pending[&1].due, 45);
        answers.observe(&Event::WindowDone { window: 0 }, 20).unwrap();
        assert!(!answers.pending.contains_key(&0));
        assert!(answers.pending.contains_key(&1));
    }

    #[test]
    fn delivery_discards_closing_or_replaced_windows() {
        for closed in [false, true] {
            let mut rt = runtime();
            let mut answers = Answers::new(Choices::default(), 0);
            for frame in 1..=16 {
                rt.tick(None);
                for event in rt.drain_events() { answers.observe(&event, frame).unwrap(); }
                if rt.window_ready(0) { break; }
            }
            assert!(rt.window_ready(0));
            assert_eq!(answers.pending.len(), 1);
            if closed { assert!(rt.answer(0, 0)); } else { answers.observe(&window(0, 2, 0), 16).unwrap(); }
            assert!(answers.deliver(&mut rt, 16).unwrap().is_empty());
            assert!(answers.pending.is_empty());
        }
    }

    #[test]
    fn choices_reject_invalid_duplicate_and_encountered_out_of_range_values() {
        for value in ["", "1", "=0", "0=", "-1=0", "1=-1", "1=256", "65536=0", "1=2=3", "1=+1", "1=2 "] {
            assert!(Choices::default().insert(value).is_err(), "{value:?}");
        }
        let mut choices = Choices::default();
        choices.insert("14=2").unwrap();
        assert!(choices.insert("014=2").is_err());
        assert_eq!(choices.answer(14, (1, 3)), Ok(2));
        assert_eq!(choices.answer(15, (1, 2)), Ok(0));
        let mut answers = Answers::new(choices, 30);
        let event = Event::Window { window: 0, entity: 1, dialog: 14, ask: Some((1, 2)), offset: 0 };
        assert!(answers.observe(&event, 1).unwrap_err().contains("out of range"));
        assert!(answers.pending.is_empty());
    }
}
