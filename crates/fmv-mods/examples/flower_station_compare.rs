//! Compare sampled original/Echo-S flower-scene paths by choice ordinal on owned local data.
//! Prints ids, flags and execution facts, never dialogue or assets. This does not enable play.
//! FMV_FF7_DATA=<data directory> cargo run --offline -p fmv-mods --example flower_station_compare -- <Echo-s.iro>
use fmv_ff7::{field::FieldFile, lgp::LgpArchive, lzss, script::{dialogue::MessageOrigin, scene::{flower_station_scene, ScenePlay, Then}}, story::StoryState};
use fmv_mods::{Layers, ModSources, Mods};
fn main() {
    let archive_path = std::env::args_os().nth(1).expect("Echo-S archive path");
    let data = std::env::var_os("FMV_FF7_DATA").expect("owned data directory");
    let mut archive = LgpArchive::open_path(std::path::Path::new(&data).join("field/flevel.lgp")).unwrap();
    let field = FieldFile::parse(lzss::decompress(&archive.read("md8_2").unwrap()).unwrap()).unwrap();
    let mods = Mods::open(&ModSources { echo_s: Some(archive_path.into()), ..Default::default() }, Layers::ALL);
    let section = mods.script_section("md8_2").unwrap();
    let def = flower_station_scene();
    for mask in 0usize..8 { for replacement in [false, true] {
        let story = StoryState::at(36);
        let mut play = if replacement { ScenePlay::with_script_section(&field, &def, &story, 1, &section).unwrap() } else { ScenePlay::new(&field, &def, &story, 1).unwrap() };
        let mut next = None;
        let mut windows = 0;
        let mut questions = 0;
        let mut voiced = 0;
        loop {
            match play.next(next.take()).then {
                Then::Window(w) => {
                    windows += 1;
                    if let MessageOrigin::Canon { dialog, .. } = w.message.origin { if replacement && mods.voice_path("md8_2", w.id, dialog, 0).is_some() { voiced += 1; } }
                    if let Some(c) = w.message.choice { let n = (mask >> questions) & 1; questions += 1; next = Some((c.first+n).min(c.last)); }
                }
                Then::End { how, .. } => {
                    println!("layer={} mask={mask} end={how:?} windows={windows} questions={questions} voiced={voiced} writes={:?} debit={} unknown={:?} unresolved={} dropped={}", if replacement {"echo"} else {"original"}, play.writes(), play.stats().ignored.contains_key("GOLDd"), play.stats().unknown, play.stats().unresolved, play.stats().dropped_requests);
                    break;
                }
            }
            assert!(windows < 64);
        }
    }}
}
