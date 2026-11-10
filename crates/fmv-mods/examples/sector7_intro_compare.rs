//! Compare the original and Echo-S Sector 7 intro with manually identified producer entities.
//! Prints ids and interpreter state only; does not enable playback or carry game content.
//! FMV_FF7_DATA=<data directory> cargo run --offline -p fmv-mods --example sector7_intro_compare -- <Echo-s.iro>
use fmv_ff7::{field::FieldFile, lgp::LgpArchive, lzss, script::{FieldScript, scene::{Limits, SceneVars, Stage, Step, Then, Vm}}, story::StoryState};
use std::collections::BTreeSet;
use fmv_mods::{Layers, ModSources, Mods};
fn main() {
    let data = std::env::var_os("FMV_FF7_DATA").unwrap();
    let mut archive = LgpArchive::open_path(std::path::Path::new(&data).join("field/flevel.lgp")).unwrap();
    let field = FieldFile::parse(lzss::decompress(&archive.read("mds7").unwrap()).unwrap()).unwrap();
    let script = FieldScript::from_field(&field).unwrap();
    let loader = field.model_loader().unwrap();
    for entity in 22usize..=31 {
        let model = &loader.models[entity-21];
        println!("cast entity={entity} name={} hrc={} idle={:?}",script.entity_name(entity).unwrap(),model.hrc_stem(),model.animations.first().map(|a| a.stem()));
    }
    let mods = Mods::open(&ModSources { echo_s: Some(std::env::args_os().nth(1).unwrap().into()), ..Default::default() }, Layers::ALL);
    let bytes = mods.script_section("mds7").unwrap();
    let echo = FieldScript::parse(&bytes).unwrap();
    for (label, script, root) in [("original", &script, 7u8), ("echo", &echo, 2u8)] {
    let mut vm = Vm::new(SceneVars::new(StoryState::at(63), &[]), 1, Limits::default());
    vm.begin(vec![Step::StartMain(root)]);
    let mut actors = BTreeSet::new();
    let mut windows = 0;
    loop {
        let beat = vm.resume(&script, None);
        for event in beat.stage { match event.what {
            Stage::Place{entity,..} | Stage::Move{entity,..} | Stage::Face{entity,..} | Stage::Anim{entity,..} | Stage::Show{entity,..} => {actors.insert(entity);},
            _ => {}
        }}
        match beat.then {
            Then::Window(_) => windows += 1,
            Then::End{how,..} => { println!("{label} end={how:?} windows={windows} actors={actors:?} writes={:?} stats={:?} unknown_reads={:?}", vm.writes(),vm.stats(),vm.vars().unknown_persistent_reads());break; }
        }
        assert!(windows<64);
    }
    }
}
