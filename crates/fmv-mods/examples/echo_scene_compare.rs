//! Compare scene script flow against an owned Echo-S archive. Prints ids and counts, never dialogue or assets.
//! FMV_FF7_DATA=<owned data> cargo run --offline -p fmv-mods --example echo_scene_compare -- <Echo-s.iro>
use fmv_ff7::field::FieldFile;
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::lzss;
use fmv_ff7::script::dialogue::MessageOrigin;
use fmv_ff7::script::scene::{post_escape_scene, proof_scene, sector7_intro_scene, sector7_station_scene, station_scene, train_scene, ScenePlay, Then};
use fmv_ff7::script::FieldScript;
use fmv_ff7::story::StoryState;
use fmv_mods::{Layers, ModSources, Mods};

fn main() {
    let archive = std::env::args_os().nth(1).expect("Echo-S archive path required");
    let data = std::env::var_os("FMV_FF7_DATA").expect("FMV_FF7_DATA required");
    let mut lgp = LgpArchive::open_path(std::path::Path::new(&data).join("field/flevel.lgp")).expect("original archive");
    let mods = Mods::open(&ModSources { echo_s: Some(archive.into()), ..Default::default() }, Layers::ALL);
    for (def, moment) in [(post_escape_scene(), 33), (station_scene(), 33), (train_scene(), 48), (sector7_station_scene(), 51), (sector7_intro_scene(), 63), (proof_scene(), 72)] {
        let raw = lzss::decompress(&lgp.read(&def.field).expect("field")).expect("decompress");
        let field = FieldFile::parse(raw).expect("field parse");
        let section = mods.script_section(&def.field).expect("Echo-S section");
        let orig_script = FieldScript::from_field(&field).unwrap();
        let echo_script = FieldScript::parse(&section).unwrap();
        if def.field == "mds7pb_1" {
            println!("{} replacement_entities={:?}", def.id, echo_script.entities().iter().enumerate().map(|(i, e)| (i, &e.name)).collect::<Vec<_>>());
        }
        for member in &def.cast {
            let name = orig_script.entity_name(usize::from(member.entity)).unwrap_or("?");
            let a = orig_script.entities().iter().filter(|e| e.name.eq_ignore_ascii_case(name)).count();
            let b = echo_script.entities().iter().filter(|e| e.name.eq_ignore_ascii_case(name)).count();
            if a != 1 || b != 1 { println!("{} entity={} name={} original_matches={} echo_matches={}", def.id, member.entity, name, a, b); }
        }
        for choice in [1, 2] {
            for replacement in [false, true] {
                let story = StoryState::at(moment);
                let built = if replacement { ScenePlay::with_script_section(&field, &def, &story, 1, &section) } else { ScenePlay::new(&field, &def, &story, 1) };
                let mut play = match built { Ok(play) => play, Err(e) => { println!("{} layer={} choice={} error={e}", def.id, if replacement {"echo"} else {"original"}, choice); continue } };
                let mut windows = Vec::new();
                let mut next_answer = None;
                let end = loop {
                    let beat = play.next(next_answer.take());
                    match beat.then {
                        Then::End { how, .. } => break how,
                        Then::Window(w) => {
                            let dialog = match w.message.origin { MessageOrigin::Canon { dialog, .. } => Some(dialog), _ => None };
                            let choices = w.message.choice.as_ref().map(|c| (c.first, c.last));
                            let voiced = replacement && dialog.is_some_and(|d| mods.voice_path(&def.field, w.id, d, 0).is_some());
                            windows.push((w.opener, dialog, choices, voiced));
                            if let Some((first, last)) = choices { next_answer = Some(first.saturating_add(choice - 1).min(last)); }
                        }
                    }
                    assert!(windows.len() <= 64, "window limit");
                };
                println!("{} layer={} choice={} end={end:?} windows={windows:?} writes={:?} stats={:?}", def.id, if replacement {"echo"} else {"original"}, choice, play.writes(), play.stats());
            }
        }
    }
}
