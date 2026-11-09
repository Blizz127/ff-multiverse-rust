//! Checks of the talk walker (`fmv_ff7::script::dialogue`) against the owner's REAL FF7 data (FF7
//! Steam Edition, English; verified 2026-11-09).
//!
//! Every test is `#[ignore]` and also returns early when `FMV_FF7_DATA` is unset, so a normal
//! `cargo test` never touches game data. Run them with
//!
//! ```text
//! FMV_FF7_DATA=<.../data> cargo test -p fmv-ff7 --test dialogue_real -- --ignored --nocapture
//! ```
//!
//! The files are only ever opened read-only. The assertions record counts, structure and the odd
//! single word; no game text is stored in this repository and none is printed.

mod script_common;

use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::script::dialogue::{
    characters, parse_message, Beat, End, Limits, MessageOrigin, MessageSource, Piece, StartOfGame, TalkError, TalkSession, Walk,
};
use fmv_ff7::script::FieldScript;
use script_common::{data_dir, flevel_path, is_field_name, load_field};

fn open() -> Option<LgpArchive<std::fs::File>> {
    let Some(dir) = data_dir() else {
        eprintln!("FMV_FF7_DATA is not set; skipping");
        return None;
    };
    Some(LgpArchive::open_path(flevel_path(&dir)).unwrap())
}

fn section_of(lgp: &mut LgpArchive<std::fs::File>, name: &str) -> Vec<u8> {
    let field = load_field(lgp, name).unwrap();
    FieldScript::from_field(&field).unwrap().bytes().to_vec()
}

/// All beats of a session, answering every question with its cursor line.
fn beats(mut session: TalkSession) -> (Vec<fmv_ff7::script::dialogue::Message>, End) {
    let mut out = Vec::new();
    for _ in 0..64 {
        match session.next(None) {
            Beat::Message(m) => out.push(m),
            Beat::End(e) => return (out, e),
        }
    }
    panic!("a conversation never ended");
}

fn plain(m: &fmv_ff7::script::dialogue::Message) -> String {
    m.page_lines().iter().map(|p| p.join("\n")).collect::<Vec<_>>().join("\n--\n")
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn md1stin_only_the_guards_have_anything_to_say_and_it_is_an_item_message() {
    let Some(mut lgp) = open() else { return };
    let section = section_of(&mut lgp, "md1stin");
    let script = FieldScript::parse(&section).unwrap();
    let chars = characters(&script);
    let mut said = Vec::new();
    for c in &chars {
        match TalkSession::talk(section.clone(), c.entity, Box::new(StartOfGame), 1) {
            Ok(session) => {
                let (messages, end) = beats(session);
                eprintln!("md1stin {} (entity {}): {} message(s), ends {end:?}", c.name, c.entity, messages.len());
                if !messages.is_empty() {
                    said.push((c.name.clone(), messages));
                }
            }
            Err(e) => eprintln!("md1stin {} (entity {}): {e}", c.name, c.entity),
        }
    }
    let names: Vec<&str> = said.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["gu0", "gu1"], "the two guards on the platform");
    for (name, messages) in &said {
        assert_eq!(messages.len(), 1, "{name}");
        let m = &messages[0];
        assert!(matches!(m.origin, MessageOrigin::Canon { .. }));
        assert!(m.choice.is_none());
        let text = plain(m);
        assert!(text.contains("Potion"), "{name} gives an item: {text:?}");
        assert!(!text.contains('{') && !text.contains('\u{fffd}'), "{name}: {text:?}");
    }
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn md1_1_nobody_talks_at_the_start_of_the_game_but_the_cutscene_lines_run_in_order() {
    let Some(mut lgp) = open() else { return };
    let section = section_of(&mut lgp, "md1_1");
    let script = FieldScript::parse(&section).unwrap();
    for c in characters(&script) {
        if let Ok(session) = TalkSession::talk(section.clone(), c.entity, Box::new(StartOfGame), 1) {
            let (messages, end) = beats(session);
            assert!(messages.is_empty(), "{} has talk text", c.name);
            assert_eq!(end, End::Finished);
        }
    }
    // The AVALANCHE members speak in scripts the director starts: Biggs (av_m) in 3 and 4, Jessie
    // (av_l) in 3. Script 4 of Biggs requests one of Cloud's (entity 1), whose line belongs
    // between his own.
    let av_m = script.entity_by_name("av_m").unwrap();
    let av_l = script.entity_by_name("av_l").unwrap();
    let cl = script.entity_by_name("cl").unwrap();
    let origins = |entity: usize, slot: usize| -> Vec<(usize, usize)> {
        let session = TalkSession::script(section.clone(), entity, slot, Box::new(StartOfGame), 1).unwrap();
        let (messages, end) = beats(session);
        assert_eq!(end, End::Finished);
        messages
            .iter()
            .map(|m| match m.origin {
                MessageOrigin::Canon { entity, dialog } => (entity, dialog),
                MessageOrigin::Added { .. } => panic!("added"),
            })
            .collect()
    };
    assert_eq!(origins(av_m, 3), [(av_m, 31)]);
    assert_eq!(origins(av_l, 3), [(av_l, 32), (av_l, 33)]);
    assert_eq!(origins(av_m, 4), [(av_m, 34), (av_m, 35), (cl, 36), (av_m, 37)]);
    // Biggs's first line: his name, then a quotation over three lines, in the Steam data's quotes.
    let session = TalkSession::script(section.clone(), av_m, 3, Box::new(StartOfGame), 1).unwrap();
    let (messages, _) = beats(session);
    let text = plain(&messages[0]);
    assert!(text.starts_with("Biggs\n\u{201c}"), "{text:?}");
    assert!(text.ends_with('\u{201d}'), "{text:?}");
    assert_eq!(text.lines().count(), 4);
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn every_character_of_every_field_can_be_walked_and_nothing_is_garbage() {
    let Some(mut lgp) = open() else { return };
    let names: Vec<String> = lgp.list().iter().filter(|e| is_field_name(&e.name)).map(|e| e.name.clone()).collect();
    let (mut fields, mut chars_total, mut talkable, mut hidden, mut with_text) = (0usize, 0usize, 0usize, 0usize, 0usize);
    let (mut budget, mut broken, mut asks, mut windows, mut window_loops) = (0usize, 0usize, 0usize, 0usize, 0usize);
    let mut ends: std::collections::BTreeMap<String, usize> = Default::default();
    let mut odd_chars: std::collections::BTreeMap<char, usize> = Default::default();
    let mut dialogs = 0usize;
    for name in &names {
        let Ok(field) = load_field(&mut lgp, name) else { continue };
        let Ok(script) = FieldScript::from_field(&field) else { continue };
        fields += 1;
        let data = script.bytes();
        // Every dialog of the field is plain text: only printable ASCII and the Steam quotes and
        // ellipsis (the bytes B2 B3 A9 B4).
        for d in script.dialogs() {
            let Some(raw) = d.offset.checked_add(d.len).and_then(|end| data.get(d.offset..end)) else { continue };
            dialogs += 1;
            for piece in parse_message(raw, &StartOfGame) {
                if let Piece::Text(t) = piece {
                    for c in t.chars() {
                        if !(' '..='~').contains(&c) && !matches!(c, '\u{201c}' | '\u{201d}' | '\u{2018}' | '\u{2026}') {
                            *odd_chars.entry(c).or_default() += 1;
                        }
                    }
                }
            }
        }
        let section = data.to_vec();
        for c in characters(&script) {
            chars_total += 1;
            match TalkSession::talk(section.clone(), c.entity, Box::new(StartOfGame), 1) {
                Err(TalkError::NotTalkable) => hidden += 1,
                Err(e) => panic!("{name} {}: {e}", c.name),
                Ok(session) => {
                    talkable += 1;
                    let mut walk_session = session;
                    let mut n = 0;
                    // Every question is answered with its last choice (the way out, usually).
                    let mut answer = None;
                    let end = loop {
                        match walk_session.next(answer.take()) {
                            Beat::Message(m) => {
                                n += 1;
                                windows += 1;
                                asks += usize::from(m.choice.is_some());
                                answer = m.choice.map(|c| c.last);
                            }
                            Beat::End(e) => break e,
                        }
                    };
                    with_text += usize::from(n > 0);
                    if end == End::Budget {
                        if n >= Limits::default().beats {
                            window_loops += 1;
                        } else {
                            eprintln!("STEP BUDGET: {name} entity {} ({}) after {n} window(s)", c.entity, c.name);
                            budget += 1;
                        }
                    }
                    broken += usize::from(end == End::Broken);
                    let key = match end {
                        End::MapJump { .. } => "MapJump".to_string(),
                        other => format!("{other:?}"),
                    };
                    *ends.entry(key).or_default() += 1;
                }
            }
        }
    }
    eprintln!(
        "{fields} fields, {dialogs} dialogs, {chars_total} characters: {hidden} hidden or silenced at the start, {talkable} talkable, {with_text} of them with text ({windows} windows, {asks} questions); ends {ends:?}; instruction budget {budget}, window cap {window_loops}, broken {broken}; odd characters {odd_chars:?}"
    );
    assert_eq!((fields, dialogs, chars_total), (702, 56_048, 2_412));
    assert!(odd_chars.is_empty(), "{odd_chars:?}");
    assert_eq!(broken, 0, "no talk script of the owned data fails to decode");
    assert_eq!(budget, 0, "no talk script of the owned data runs out of instructions");
    // Measured 2026-11-09 with the start-of-game hook and every question answered with its last choice.
    assert_eq!((hidden, talkable, with_text), (922, 1_490, 771));
    assert!(window_loops <= 3, "{window_loops}: questions that loop for ever (answered 'last' every time)");
}

/// The lines of the last page of a dialog (a page break restarts the count), each as the bytes of
/// the line (control arguments skipped), for the `ASK` measurements.
fn last_page_lines(raw: &[u8]) -> Vec<Vec<u8>> {
    let mut lines: Vec<Vec<u8>> = vec![vec![]];
    let mut i = 0;
    while let Some(&b) = raw.get(i) {
        i += 1;
        match b {
            0xFF => break,
            0xE7 => lines.push(vec![]),
            0xE8 | 0xE9 => lines = vec![vec![]],
            0xFE => i += 1 + match raw.get(i).copied().unwrap_or(0) { 0xDD => 2, 0xE2 => 4, _ => 0 },
            0xFA..=0xFD => i += 1,
            other => {
                if let Some(line) = lines.last_mut() {
                    line.push(other);
                }
            }
        }
    }
    lines
}

/// The facts about `ASK` that the module docs and the walker rely on, measured over every distinct
/// script of every field: counts only.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_facts_about_ask_that_the_walker_relies_on() {
    let Some(mut lgp) = open() else { return };
    let names: Vec<String> = lgp.list().iter().filter(|e| is_field_name(&e.name)).map(|e| e.name.clone()).collect();
    let (mut asks, mut unmarked, mut beyond) = (0usize, 0usize, 0usize);
    let (mut preset_in, mut preset_other, mut no_preset) = (0usize, 0usize, 0usize);
    let (mut absolute, mut relative, mut both, mut unknown) = (0usize, 0usize, 0usize, 0usize);
    for name in &names {
        let Ok(field) = load_field(&mut lgp, name) else { continue };
        let Ok(script) = FieldScript::from_field(&field) else { continue };
        let mut seen = std::collections::BTreeSet::new();
        for entity in 0..script.entity_count() {
            for slot in 0..32 {
                let Some(span) = script.span(entity, slot) else { continue };
                if !seen.insert(span.start) {
                    continue;
                }
                let Ok(listing) = script.disassemble_partial(entity, slot) else { continue };
                for (k, ins) in listing.instructions.iter().enumerate().filter(|(_, i)| i.mnemonic == "ASK") {
                    let (Some(dialog), Some(first), Some(last)) = (ins.literal("dialog"), ins.literal("first"), ins.literal("last")) else { continue };
                    let Some(d) = usize::try_from(dialog).ok().and_then(|d| script.dialogs().get(d)) else { continue };
                    let Some(raw) = d.offset.checked_add(d.len).and_then(|end| script.bytes().get(d.offset..end)) else { continue };
                    asks += 1;
                    // Which lines are choices: do they carry the cursor code, after their blanks?
                    let lines = last_page_lines(raw);
                    let (f, l) = (usize::try_from(first).unwrap(), usize::try_from(last).unwrap());
                    beyond += usize::from(l >= lines.len());
                    let marked = |n: usize| lines.get(n).is_some_and(|line| line.iter().find(|&&b| b != 0xE1 && b != 0x00) == Some(&0xE0));
                    unmarked += usize::from(!(f..=l).all(marked));
                    // Does the script put the cursor with a SETBYTE just before? And how does it compare the answer?
                    let var = ins.operand("var").map(|v| (v.bank, v.value & 0xFF));
                    let preset = listing.instructions.get(..k).unwrap_or(&[]).iter().rev().take(8).find(|p| {
                        matches!(p.mnemonic, "SETBYTE" | "SETWORD") && p.operand("var").map(|v| (v.bank, v.value & 0xFF)) == var
                    });
                    match preset.and_then(|p| p.literal("value")) {
                        Some(v) if (first..=last).contains(&v) => preset_in += 1,
                        Some(_) => preset_other += 1,
                        None => no_preset += 1,
                    }
                    let (mut abs_hit, mut rel_hit) = (false, false);
                    for q in listing.instructions.get(k + 1..).unwrap_or(&[]).iter().take(30) {
                        if !matches!(q.mnemonic, "IFUB" | "IFUBL" | "IFSW" | "IFSWL" | "IFUW" | "IFUWL") {
                            continue;
                        }
                        let left = q.operand("left").filter(|o| o.bank != 0).map(|o| (o.bank, o.value & 0xFF));
                        let (Some(right), true) = (q.literal("right"), left == var) else { continue };
                        let n = last - first;
                        abs_hit |= right >= first && right <= last && right > n;
                        rel_hit |= right >= 0 && right <= n && right < first;
                    }
                    match (abs_hit, rel_hit) {
                        (true, false) => absolute += 1,
                        (false, true) => relative += 1,
                        (true, true) => both += 1,
                        _ => unknown += 1,
                    }
                }
            }
        }
    }
    eprintln!("ASKs {asks}: unmarked {unmarked}, last line beyond the text {beyond}; preset in range {preset_in}, other {preset_other}, none {no_preset}; answers compared as line numbers {absolute}, indexes {relative}, both {both}, unknown {unknown}");
    // Measured 2026-11-09 (see the module docs of `fmv_ff7::script::dialogue`).
    assert_eq!(asks, 908);
    assert_eq!((absolute, relative, both, unknown), (465, 5, 20, 418), "the answer is the absolute line number of the choice");
    assert_eq!((preset_in, preset_other, no_preset), (387, 335, 186), "scripts set the cursor line before asking");
    assert_eq!(unmarked, 251, "choice lines do not always carry the cursor code");
    assert_eq!(beyond, 20, "some name a last line the text does not have");
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_walker_is_deterministic_and_bounded_on_every_script_slot_of_two_fields() {
    let Some(mut lgp) = open() else { return };
    for name in ["md1stin", "md1_1"] {
        let section = section_of(&mut lgp, name);
        let script = FieldScript::parse(&section).unwrap();
        for entity in 0..script.entity_count() {
            for slot in 0..32 {
                let run = |seed: u32| {
                    let mut walk = Walk::script(entity, slot, seed, Limits::default());
                    let mut out = Vec::new();
                    for _ in 0..40 {
                        match walk.resume(&script, &StartOfGame, None) {
                            Beat::Message(m) => out.push(Some(m)),
                            Beat::End(_) => {
                                out.push(None);
                                break;
                            }
                        }
                    }
                    assert!(walk.steps() <= Limits::default().steps);
                    out
                };
                assert_eq!(run(5), run(5), "{name} entity {entity} slot {slot}");
            }
        }
    }
}
