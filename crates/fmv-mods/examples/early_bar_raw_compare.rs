//! Compare the early bar director's original and Echo-S script paths, using owned local data.
//! Prints ids, execution stats and variable values, not game dialogue or assets. This is a raw
//! interpreter survey with manually identified slots; it does not enable or validate playback.
//!
//! FMV_FF7_DATA=<data directory> cargo run --offline -p fmv-mods --example early_bar_raw_compare -- <Echo-s.iro>
use fmv_ff7::{field::FieldFile, lgp::LgpArchive, lzss, script::{FieldScript, dialogue::MessageOrigin, scene::{Limits, SceneVars, ScriptRef, Step, Then, Vm}}, story::StoryState};
use fmv_mods::{Layers, ModSources, Mods};
fn main() {
    let data = std::env::var_os("FMV_FF7_DATA").unwrap();
    let mut archive = LgpArchive::open_path(std::path::Path::new(&data).join("field/flevel.lgp")).unwrap();
    let field = FieldFile::parse(lzss::decompress(&archive.read("mds7pb_1").unwrap()).unwrap()).unwrap();
    let archive_path = std::env::args_os().nth(1).expect("Echo-S archive path");
    let mods = Mods::open(&ModSources { echo_s: Some(archive_path.into()), ..Default::default() }, Layers::ALL);
    let section = mods.script_section("mds7pb_1").unwrap();
    let original = FieldScript::from_field(&field).unwrap();
    let echo = FieldScript::parse(&section).unwrap();
    for (label, script, setup, root) in [("original", &original, ScriptRef::new(10,1), ScriptRef::new(7,1)), ("echo", &echo, ScriptRef::new(2,3), ScriptRef::new(2,1))] {
        for answer in [0usize, 1] {
            let mut vm = Vm::new(SceneVars::new(StoryState::at(63), &[]), 1, Limits::default());
            vm.setup(script, &[setup]);
            let setup_stats = vm.take_stats();
            vm.begin(vec![Step::Await(root)]);
            let mut next = None;
            let mut windows = Vec::new();
            loop {
                match vm.resume(script, next.take()).then {
                    Then::Window(w) => {
                        let id = match w.message.origin { MessageOrigin::Canon { dialog, .. } => Some(dialog), _ => None };
                        let range = w.message.choice.as_ref().map(|c| (c.first,c.last));
                        let voice = id.is_some_and(|d| label == "echo" && mods.voice_path("mds7pb_1", w.id, d, 0).is_some());
                        windows.push((id,range,voice));
                        if let Some((first,last)) = range { next = Some((first+answer).min(last)); }
                    }
                    Then::End { how, .. } => {
                        println!("{label} answer={answer} end={how:?} windows={windows:?} writes={:?} setup={setup_stats:?} stats={:?} unknown_reads={:?}", vm.writes(), vm.stats(), vm.vars().unknown_persistent_reads());
                        break;
                    }
                }
                assert!(windows.len()<64);
            }
        }
    }
}
