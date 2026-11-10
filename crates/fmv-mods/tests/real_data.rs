//! Checks of the mod layers against the owner's REAL files: Echo-S 1.24 (`Echo-s.iro`), Cosmo Memory 1.22
//! (`[Tsunamods]_Cosmo_Memory.iro`) and, for the text and the music names, the owner's FF7 Steam Edition.
//!
//! Every test is `#[ignore]` and also returns early when its files are not named, so a normal `cargo test` never touches
//! them. Run them with
//!
//! ```text
//! FMV_ECHO_S_IRO=<.../Echo-s.iro> FMV_COSMO_IRO=<.../[Tsunamods]_Cosmo_Memory.iro> \
//! FMV_FF7_DATA=<.../data> FMV_FF7_AUDIO_DATA=<...-audio/data> \
//!   cargo test --offline --release -p fmv-mods --test real_data -- --ignored --nocapture
//! ```
//!
//! Everything is opened read-only, nothing is extracted or written, and nothing here prints or stores a byte or a line of text
//! of either archive or of the game: counts, sizes and archive paths only.

use fmv_ff7::field::FieldFile;
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::lzss;
use fmv_ff7::music::MusicList;
use fmv_ff7::script::dialogue::{characters, find_character, parse_message, Beat, MessageSource, Piece, StartOfGame, TalkSession};
use fmv_ff7::script::FieldScript;
use fmv_mods::compare::{align, message_lines, Alignment};
use fmv_mods::{IroArchive, Layers, ModSources, Mods};
use std::path::PathBuf;

/// The first reactor mission's fields, in the order of `scripts/travel-smoke.sh --chain`.
const REACTOR_FIELDS: [&str; 10] = ["md1stin", "md1_1", "md1_2", "nrthmk", "nmkin_1", "elevtr1", "nmkin_2", "nmkin_3", "nmkin_4", "nmkin_5"];

fn path_of(var: &str) -> Option<PathBuf> {
    let path = std::env::var_os(var).map(PathBuf::from);
    if path.is_none() {
        eprintln!("{var} is not set; skipping");
    }
    path
}

fn mods() -> Option<Mods> {
    let echo = path_of("FMV_ECHO_S_IRO")?;
    let cosmo = std::env::var_os("FMV_COSMO_IRO").map(PathBuf::from);
    let m = Mods::open(&ModSources { echo_s: Some(echo), cosmo, dir: None }, Layers::ALL);
    assert!(m.notes().is_empty(), "opening said: {:?}", m.notes());
    Some(m)
}

fn original_flevel() -> Option<LgpArchive<std::fs::File>> {
    let dir = path_of("FMV_FF7_DATA")?;
    Some(LgpArchive::open_path(dir.join("field").join("flevel.lgp")).expect("the original flevel.lgp"))
}

fn original_script(lgp: &mut LgpArchive<std::fs::File>, field: &str) -> Vec<u8> {
    let raw = lzss::decompress(&lgp.read(field).expect("a field of the original")).expect("LZSS");
    let file = FieldFile::parse(raw).expect("field file");
    FieldScript::from_field(&file).expect("script").bytes().to_vec()
}

#[test]
#[ignore = "needs the owner's mod archives (FMV_ECHO_S_IRO, FMV_COSMO_IRO)"]
fn the_index_of_both_archives() {
    let Some(echo) = path_of("FMV_ECHO_S_IRO") else { return };
    let a = IroArchive::open_path(&echo).expect("Echo-S opens: a truncated download is refused by entry name");
    assert_eq!((a.version(), a.archive_flags()), (0x1_0002, 0));
    assert_eq!(a.len(), 23_178, "entries of Echo-s.iro 1.24");
    assert_eq!(a.file_len(), 4_335_170_582);
    let end = a.entries().iter().map(|e| e.offset + u64::from(e.length)).max().unwrap_or(0);
    assert_eq!(end, a.file_len(), "the last entry ends exactly at the end of the file: nothing is cut off");
    assert!(a.entries().iter().all(|e| e.compression() == fmv_mods::Compression::Stored), "every entry is stored");
    assert_eq!(a.duplicate_names(), 0);
    let count = |prefix: &str| a.entries().iter().filter(|e| fmv_mods::normalize(&e.name).starts_with(&fmv_mods::normalize(prefix))).count();
    assert_eq!(count("Echo-S\\flevel.lgp\\"), 706);
    assert_eq!(count("VO\\Voice\\"), 15_461);
    assert_eq!(count("EchoeS Of The Planet\\vgmstream\\"), 112);
    assert_eq!(count("Arranged\\vgmstream\\"), 10);
    assert_eq!(count("Sound\\sfx\\"), 881);
    println!("Echo-S: {} entries, {} bytes, complete, all stored", a.len(), a.file_len());

    let Some(cosmo) = path_of("FMV_COSMO_IRO") else { return };
    let c = IroArchive::open_path(&cosmo).expect("Cosmo opens");
    assert_eq!(c.version(), 0x1_0002);
    assert_eq!(c.len(), 1_610, "entries of [Tsunamods]_Cosmo_Memory.iro 1.22");
    assert_eq!(c.file_len(), 333_643_683);
    let end = c.entries().iter().map(|e| e.offset + u64::from(e.length)).max().unwrap_or(0);
    assert_eq!(end, c.file_len());
    assert!(c.entries().iter().all(|e| e.compression() == fmv_mods::Compression::Stored));
    let count = |prefix: &str| c.entries().iter().filter(|e| fmv_mods::normalize(&e.name).starts_with(&fmv_mods::normalize(prefix))).count();
    assert_eq!(count("Base\\sfx\\"), 1_001);
    assert_eq!(count("V_Attacks\\sfx\\"), 170);
    assert_eq!(count("NV_Attacks\\sfx\\"), 171);
    assert_eq!(count("UI_Dissidia\\sfx\\"), 16);
    assert_eq!(count("Ambience\\Ambient\\"), 119);
    println!("Cosmo Memory: {} entries, {} bytes, complete, all stored", c.len(), c.file_len());

    // Discovery by the mod.xml ID finds both from the folders they sit in, whatever their names.
    let dir = echo.parent().and_then(|p| p.parent()).map(PathBuf::from).expect("the assets folder");
    let found = Mods::open(&ModSources { dir: Some(dir), ..Default::default() }, Layers::ALL);
    assert!(found.has_echo() && found.has_cosmo(), "{:?}", found.notes());
    println!("{}", found.describe());
}

/// How many windows a character's conversation opens at the start of a new game (`None`: it cannot be talked to then).
fn conversation_windows(section: &[u8], entity: usize) -> Option<usize> {
    let mut session = TalkSession::talk(section.to_vec(), entity, Box::new(StartOfGame), 1).ok()?;
    let mut windows = 0;
    for _ in 0..64 {
        match session.next(None) {
            Beat::Message(_) => windows += 1,
            Beat::End(_) => break,
        }
    }
    Some(windows)
}

#[test]
#[ignore = "needs the owner's Echo-S archive and FF7 data (FMV_ECHO_S_IRO, FMV_FF7_DATA)"]
fn echo_s_text_covers_the_reactor_fields() {
    let (Some(m), Some(mut lgp)) = (mods(), original_flevel()) else { return };
    let (mut covered, mut npcs, mut npcs_found) = (0, 0, 0);
    let (mut dialogs_orig, mut dialogs_echo) = (0usize, 0usize);
    let (mut lines_orig, mut lines_echo) = (0usize, 0usize);
    let mut total = Alignment::default();
    let (mut convs_echo, mut convs_fallback, mut windows_echo, mut windows_fallback) = (0usize, 0usize, 0usize, 0usize);
    for field in REACTOR_FIELDS {
        let orig = original_script(&mut lgp, field);
        let os = FieldScript::parse(&orig).expect("original script");
        let Some(section) = m.script_section(field) else {
            println!("{field}: NO Echo-S section");
            continue;
        };
        covered += 1;
        let es = FieldScript::parse(&section).expect("Echo-S script");
        dialogs_orig += os.dialogs().len();
        dialogs_echo += es.dialogs().len();
        let (lo, le) = (message_lines(&os).len(), message_lines(&es).len());
        lines_orig += lo;
        lines_echo += le;
        let a = align(&os, &es);
        total.original_lines += a.original_lines;
        total.lines_matched += a.lines_matched;
        total.lines_same_number += a.lines_same_number;
        // The NPCs the client would talk to: found in Echo-S's section by name and place, else the original plays.
        let (oc, ec) = (characters(&os), characters(&es));
        let (mut f_found, mut f_echo, mut f_fallback) = (0, 0usize, 0usize);
        for c in &oc {
            let place = c.places.first().map_or((0.0, 0.0), |&(x, y)| (f32::from(x), f32::from(y)));
            let found = find_character(&ec, &c.name, place.0, place.1);
            f_found += usize::from(found.is_some());
            let original = conversation_windows(&orig, c.entity);
            let echo = found.and_then(|f| conversation_windows(&section, f.entity));
            match (original, echo) {
                (_, Some(w)) => {
                    convs_echo += 1;
                    windows_echo += w;
                    f_echo += w;
                }
                (Some(w), None) => {
                    convs_fallback += 1;
                    windows_fallback += w;
                    f_fallback += w;
                }
                (None, None) => {}
            }
        }
        npcs += oc.len();
        npcs_found += f_found;
        println!(
            "{field}: dialog strings {} -> {}; message lines opened by scripts {lo} -> {le} (original lines in a script the same in Echo-S: {}/{}); \
             NPCs found in Echo-S {f_found}/{}; talk windows from Echo-S {f_echo}, from the original (NPC not talkable in Echo-S) {f_fallback}",
            os.dialogs().len(),
            es.dialogs().len(),
            a.lines_matched,
            a.original_lines,
            oc.len(),
        );
    }
    println!(
        "reactor fields: {covered}/{} covered by an Echo-S script; dialog strings {dialogs_orig} -> {dialogs_echo}; message lines opened: original {lines_orig}, Echo-S {lines_echo}; \
         original lines in a script that is the same in Echo-S: {}/{} ({} renumbered); NPCs found {npcs_found}/{npcs}; \
         talk conversations from Echo-S {convs_echo} ({windows_echo} windows), the original's fallback {convs_fallback} ({windows_fallback} windows)",
        REACTOR_FIELDS.len(),
        total.lines_matched,
        total.original_lines,
        total.lines_matched - total.lines_same_number,
    );
    assert_eq!(covered, REACTOR_FIELDS.len(), "Echo-S has a script for every reactor field");
    assert!(npcs_found * 10 >= npcs * 8, "at least 80% of the NPCs are found in Echo-S's script by name and place");
    assert!(lines_echo > 0 && convs_echo > 0);
}

/// The game pages of dialog `dialog` of `script` (1 + its page breaks).
fn pages_of(script: &FieldScript<'_>, dialog: usize) -> usize {
    let Some(d) = script.dialogs().get(dialog) else { return 1 };
    let Some(raw) = script.bytes().get(d.offset..d.offset + d.len) else { return 1 };
    1 + parse_message(raw, &StartOfGame).iter().filter(|p| matches!(p, Piece::NewPage)).count()
}

#[test]
#[ignore = "needs the owner's Echo-S archive (FMV_ECHO_S_IRO)"]
fn voice_clips_are_found_per_line_of_the_reactor_fields() {
    let Some(m) = mods() else { return };
    let (mut lines, mut voiced, mut pages, mut pages_voiced) = (0usize, 0usize, 0usize, 0usize);
    let (mut asks, mut option_lines, mut option_voiced) = (0usize, 0usize, 0usize);
    let (mut dialogs_total, mut dialogs_voiced) = (0usize, 0usize);
    for field in REACTOR_FIELDS {
        let section = m.script_section(field).expect("Echo-S section");
        let es = FieldScript::parse(&section).expect("script");
        let (mut f_lines, mut f_voiced) = (0, 0);
        for (window, dialog) in message_lines(&es) {
            f_lines += 1;
            let n = pages_of(&es, dialog);
            let hit: Vec<bool> = (0..n).map(|p| m.voice_path(field, window, dialog, p).is_some()).collect();
            pages += n;
            pages_voiced += hit.iter().filter(|h| **h).count();
            if hit.iter().any(|h| *h) {
                f_voiced += 1;
            }
        }
        // The questions: every `ASK`'s lines, with the option voice FFNx plays as the cursor reaches each.
        for entity in 0..es.entity_count() {
            for slot in 0..fmv_ff7::script::SCRIPTS_PER_ENTITY {
                let Ok(d) = es.disassemble_partial(entity, slot) else { continue };
                for ins in d.instructions.iter().filter(|i| i.opcode == 0x48) {
                    let (Some(dialog), Some(first), Some(last)) = (ins.literal("dialog"), ins.literal("first"), ins.literal("last")) else { continue };
                    asks += 1;
                    for line in first..=last {
                        option_lines += 1;
                        let (Ok(dialog), Ok(line)) = (usize::try_from(dialog), usize::try_from(line)) else { continue };
                        option_voiced += usize::from(m.option_voice(field, dialog, line).is_some());
                    }
                }
            }
        }
        lines += f_lines;
        voiced += f_voiced;
        // And every string of the table, asked as window 0, page 0.
        for d in 0..es.dialogs().len() {
            dialogs_total += 1;
            dialogs_voiced += usize::from(m.voice_path(field, 0, d, 0).is_some());
        }
        println!("{field}: message lines {f_lines}, with a voice {f_voiced}");
    }
    println!(
        "reactor fields: {lines} message lines (window+dialog opened by the script), {voiced} with a voice on at least one page; {pages} pages, {pages_voiced} voiced; \
         {asks} questions with {option_lines} choice lines, {option_voiced} with an option voice; table strings {dialogs_total}, {dialogs_voiced} voiced as window 0 page 0"
    );
    assert!(lines > 0 && voiced > 0);
}

/// Is the clip of a question's line named by the line's ABSOLUTE number (what `first..=last` of the `ASK` counts, which the cursor reaches) or by
/// its number from the first choice? Scans every `ASK` with literal operands in all of Echo-S's scripts and counts the files each reading finds.
#[test]
#[ignore = "needs the owner's Echo-S archive (FMV_ECHO_S_IRO)"]
fn option_clips_are_named_by_the_absolute_line_of_the_question() {
    let (Some(m), Some(path)) = (mods(), path_of("FMV_ECHO_S_IRO")) else { return };
    let archive = IroArchive::open_path(&path).expect("Echo-S");
    let fields: Vec<String> = archive
        .entries()
        .iter()
        .filter(|e| e.folder().eq_ignore_ascii_case("Echo-S\\flevel.lgp") && !e.file_name().contains('.'))
        .map(|e| e.file_name().to_ascii_lowercase())
        .collect();
    let (mut asks, mut asks_from_zero) = (0usize, 0usize);
    // For a question whose first choice is line 0 the two numberings are the same; for the others they differ.
    let (mut zero_lines, mut zero_hits) = (0usize, 0usize);
    let (mut lines, mut absolute, mut relative, mut both, mut neither) = (0usize, 0usize, 0usize, 0usize, 0usize);
    for field in &fields {
        let Some(section) = m.script_section(field) else { continue };
        let script = FieldScript::parse(&section).expect("script");
        for entity in 0..script.entity_count() {
            for slot in 0..fmv_ff7::script::SCRIPTS_PER_ENTITY {
                let Ok(d) = script.disassemble_partial(entity, slot) else { continue };
                for ins in d.instructions.iter().filter(|i| i.opcode == 0x48) {
                    let (Some(dialog), Some(first), Some(last)) = (ins.literal("dialog"), ins.literal("first"), ins.literal("last")) else { continue };
                    let (Ok(dialog), Ok(first), Ok(last)) = (usize::try_from(dialog), usize::try_from(first), usize::try_from(last)) else { continue };
                    asks += 1;
                    asks_from_zero += usize::from(first == 0);
                    for k in first..=last.max(first) {
                        let (abs, rel) = (m.option_voice(field, dialog, k).is_some(), m.option_voice(field, dialog, k - first).is_some());
                        if first == 0 {
                            zero_lines += 1;
                            zero_hits += usize::from(abs);
                            continue;
                        }
                        lines += 1;
                        match (abs, rel) {
                            (true, true) => both += 1,
                            (true, false) => absolute += 1,
                            (false, true) => relative += 1,
                            (false, false) => neither += 1,
                        }
                    }
                }
            }
        }
    }
    println!(
        "{asks} questions in {} fields ({asks_from_zero} start at line 0: {zero_lines} lines, {zero_hits} with a clip); \
         for the {lines} lines of the others: a clip by the absolute number only {absolute}, by the number from the first choice only {relative}, either {both}, none {neither}",
        fields.len()
    );
    assert!(asks > 0);
    // Measured on 1.24: nearly every question starts its choices at line 0 (where the two numberings are one), three quarters of those lines have
    // a clip, and no line of the others has one under either numbering: nothing contradicts FFNx's reading (the absolute line).
    assert!(asks_from_zero * 10 >= asks * 9 && zero_hits * 2 > zero_lines);
    assert_eq!((absolute, relative), (0, 0), "a clip named by one numbering only would say the other is the real one");
}

#[test]
#[ignore = "needs the owner's mod archives and audio data (FMV_ECHO_S_IRO, FMV_COSMO_IRO, FMV_FF7_AUDIO_DATA)"]
fn music_and_sound_layers_cover_these_names_and_ids() {
    let (Some(m), Some(audio)) = (mods(), path_of("FMV_FF7_AUDIO_DATA")) else { return };
    let idx = std::fs::read(audio.join("music").join("music.idx")).expect("music.idx");
    let list = MusicList::parse(&idx);
    let names: Vec<&str> = list.names().iter().map(String::as_str).collect();
    let replaced: Vec<&str> = names.iter().copied().filter(|n| m.music(n).is_some()).collect();
    println!("music.idx names {}, Echo-S has a track for {}, the original plays for {}", names.len(), replaced.len(), names.len() - replaced.len());
    assert!(!replaced.is_empty());
    let (mut with_clips, mut clip_total) = (0usize, 0usize);
    for id in 1..=750u16 {
        if let Some(set) = m.sfx(id, None) {
            with_clips += 1;
            clip_total += set.clips.len();
        }
    }
    println!("sound ids 1..=750: {with_clips} have clips from the sound layer ({clip_total} clips in all), {} play the original", 750 - with_clips);
    assert!(with_clips > 600, "Cosmo's config names clips for the vast majority of the ids");
}

#[test]
#[ignore = "needs the owner's Echo-S archive (FMV_ECHO_S_IRO); opening is not enabled"]
fn reactor_opening_echo_s_changes_entry_moment_and_has_unsafe_tail() {
    use fmv_ff7::script::dialogue::{End, MessageOrigin};
    use fmv_ff7::script::scene::{Limits, SceneVars, Step, Then, Vm};
    use fmv_ff7::story::StoryState;
    let m = mods().expect("FMV_ECHO_S_IRO");
    let bytes = m.script_section("md1stin").expect("Echo-S md1stin section");
    let script = FieldScript::parse(&bytes).unwrap();
    assert!(script.disassemble_partial(0, 0).unwrap().instructions.iter().any(|i|
        i.mnemonic == "SETWORD" && i.operand("var").is_some_and(|v| (v.bank, v.value) == (2, 0)) && i.literal("value") == Some(2)
    ));
    let run = |moment| {
        let mut vm = Vm::new(SceneVars::new(StoryState::at(moment), &[]), 1, Limits::default());
        vm.begin(vec![Step::StartMain(0)]);
        let mut lines = Vec::new();
        let end = loop {
            match vm.resume(&script, None).then {
                Then::Window(window) => lines.push((window.id, window.opener, window.message.origin)),
                Then::End { how, .. } => break how,
            }
            assert!(lines.len() <= 16);
        };
        (end, lines, vm)
    };
    let (end, lines, _) = run(0);
    assert_eq!(end, End::MapJump { field: 109 });
    assert!(lines.is_empty());
    let (end, lines, vm) = run(1);
    assert_eq!(end, End::Battle);
    assert_eq!(lines.len(), 1);
    assert!(matches!(&lines[0], (3, 2, MessageOrigin::Canon { entity: 2, dialog: 6 })));
    assert!(m.voice_path("md1stin", 3, 6, 0).is_some());
    assert!(vm.stats().handled.contains_key("GETAI"));
    assert!(vm.stats().unresolved > 0);
    for op in ["PRTYE", "MMBLK", "HMPMAX1", "MOVIE", "PMVIE", "SOUND"] {
        assert!(vm.stats().ignored.contains_key(op), "{op}");
    }
    assert_eq!(vm.writes().get(&(1, 4)), Some(&30));
    assert!(vm.vars().unknown_persistent_reads().contains(&(1, 4)));
}
