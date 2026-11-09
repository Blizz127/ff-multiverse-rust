//! Does the dialogue walker work on Echo-S's field scripts as it does on the original's? Counts only, never text.
//!
//! ```text
//! FMV_FF7_DATA=<.../data> cargo run --offline --release -p fmv-mods --example echo_talk_compare -- <Echo-s.iro>
//! ```
//!
//! For every field both have: decode failures in either script section; every original character the client
//! would talk to (`find_character` by name and place) found in Echo-S's section; and the same conversation
//! walked on both sections at the start of a new game: talkable or not, the number of windows, the questions.

use fmv_ff7::field::FieldFile;
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::lzss;
use fmv_ff7::script::dialogue::{characters, find_character, Beat, End, MessageSource, StartOfGame, TalkSession};
use fmv_ff7::script::{FieldScript, SCRIPTS_PER_ENTITY};
use fmv_mods::IroArchive;
use std::collections::{BTreeMap, BTreeSet};

fn load(bytes: &[u8]) -> Result<FieldFile, String> {
    let raw = lzss::decompress(bytes).map_err(|e| e.to_string())?;
    FieldFile::parse(raw).map_err(|e| e.to_string())
}

#[derive(Debug, PartialEq, Eq, Clone)]
struct Run {
    windows: usize,
    questions: usize,
    end: String,
}

fn walk(section: &[u8], entity: usize) -> Option<Run> {
    let mut session = TalkSession::talk(section.to_vec(), entity, Box::new(StartOfGame), 1).ok()?;
    let (mut windows, mut questions) = (0, 0);
    for _ in 0..64 {
        match session.next(None) {
            Beat::Message(m) => {
                windows += 1;
                if m.choice.is_some() {
                    questions += 1;
                }
            }
            Beat::End(e) => {
                let end = match e {
                    End::Finished => "finished",
                    End::Budget => "budget",
                    End::Broken => "broken",
                    End::MapJump { .. } => "mapjump",
                    End::Battle => "battle",
                    End::MiniGame => "minigame",
                    End::GameOver => "gameover",
                };
                return Some(Run { windows, questions, end: end.to_string() });
            }
        }
    }
    Some(Run { windows, questions, end: "64 windows".into() })
}

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(iro_path) = args.next() else {
        eprintln!("usage: echo_talk_compare <Echo-s.iro>   (with FMV_FF7_DATA set)");
        std::process::exit(2);
    };
    let Some(data) = std::env::var_os("FMV_FF7_DATA") else {
        eprintln!("FMV_FF7_DATA is not set");
        std::process::exit(2);
    };
    let only: Vec<String> = args.collect();
    let flevel = std::path::Path::new(&data).join("field").join("flevel.lgp");
    let mut orig = LgpArchive::open_path(&flevel).expect("open the original flevel.lgp");
    let echo = IroArchive::open_path(&iro_path).expect("open the Echo-S archive");
    let names: BTreeSet<String> = orig.list().iter().map(|e| e.name.to_ascii_lowercase()).filter(|n| !n.contains('.') && n != "maplist").collect();

    let (mut fields, mut decode_err_orig, mut decode_err_echo, mut field_err) = (0, 0, 0, 0);
    let (mut scripts_orig, mut scripts_echo) = (0usize, 0usize);
    let (mut chars_orig, mut chars_found) = (0usize, 0usize);
    let mut chars_missing_by_field: BTreeMap<String, usize> = BTreeMap::new();
    let (mut talk_both, mut talk_only_orig, mut talk_only_echo, mut talk_neither) = (0, 0, 0, 0);
    let (mut same_windows, mut diff_windows, mut same_questions) = (0, 0, 0);
    let mut ends: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut echo_ends: BTreeMap<String, usize> = BTreeMap::new();
    let mut orig_ends: BTreeMap<String, usize> = BTreeMap::new();
    let mut echo_windows_total = 0usize;
    let mut orig_windows_total = 0usize;
    for name in &names {
        if !only.is_empty() && !only.contains(name) {
            continue;
        }
        let (Ok(ob), Ok(eb)) = (orig.read(name), echo.read_named(&format!("Echo-S\\flevel.lgp\\{name}"))) else {
            field_err += 1;
            continue;
        };
        let (Ok(of), Ok(ef)) = (load(&ob), load(&eb)) else {
            field_err += 1;
            continue;
        };
        let (Ok(os), Ok(es)) = (FieldScript::from_field(&of), FieldScript::from_field(&ef)) else {
            field_err += 1;
            continue;
        };
        fields += 1;
        for (script, errs, count) in [(&os, &mut decode_err_orig, &mut scripts_orig), (&es, &mut decode_err_echo, &mut scripts_echo)] {
            for ent in 0..script.entity_count() {
                for slot in 0..SCRIPTS_PER_ENTITY {
                    if let Ok(d) = script.disassemble_partial(ent, slot) {
                        *count += 1;
                        if d.error.is_some() {
                            *errs += 1;
                        }
                    }
                }
            }
        }
        let (oc, ec) = (characters(&os), characters(&es));
        for c in &oc {
            chars_orig += 1;
            let place = c.places.first().map(|&(x, y)| (f32::from(x), f32::from(y))).unwrap_or((0.0, 0.0));
            let found = find_character(&ec, &c.name, place.0, place.1);
            if found.is_some() {
                chars_found += 1;
            } else {
                *chars_missing_by_field.entry(name.clone()).or_default() += 1;
            }
            let ra = walk(os.bytes(), c.entity);
            let rb = found.and_then(|f| walk(es.bytes(), f.entity));
            match (&ra, &rb) {
                (Some(a), Some(b)) => {
                    talk_both += 1;
                    if a.windows == b.windows {
                        same_windows += 1;
                    } else {
                        diff_windows += 1;
                    }
                    if a.questions == b.questions {
                        same_questions += 1;
                    }
                    *ends.entry((a.end.clone(), b.end.clone())).or_default() += 1;
                    orig_windows_total += a.windows;
                    echo_windows_total += b.windows;
                    *orig_ends.entry(a.end.clone()).or_default() += 1;
                    *echo_ends.entry(b.end.clone()).or_default() += 1;
                }
                (Some(_), None) => talk_only_orig += 1,
                (None, Some(_)) => talk_only_echo += 1,
                (None, None) => talk_neither += 1,
            }
        }
    }
    println!("fields compared: {fields} (a field that would not load: {field_err})");
    println!("scripts decoded: original {scripts_orig} (decode errors {decode_err_orig}), Echo-S {scripts_echo} (decode errors {decode_err_echo})");
    println!("original characters: {chars_orig}; found in Echo-S's section by name and place: {chars_found}; missing: {}", chars_orig - chars_found);
    println!("fields with a missing character (top): {:?}", chars_missing_by_field.iter().take(10).collect::<Vec<_>>());
    println!("conversations at the start of a new game: both talkable {talk_both}, only the original {talk_only_orig}, only Echo-S {talk_only_echo}, neither {talk_neither}");
    println!("  among both: same window count {same_windows}, different {diff_windows}; same question count {same_questions}");
    println!("  windows in total: original {orig_windows_total}, Echo-S {echo_windows_total}");
    println!("  how they end (original): {orig_ends:?}");
    println!("  how they end (Echo-S)  : {echo_ends:?}");
    let mut e: Vec<_> = ends.iter().collect();
    e.sort_by(|a, b| b.1.cmp(a.1));
    println!("  end pairs (original, Echo-S): {:?}", e.into_iter().take(8).collect::<Vec<_>>());
}
