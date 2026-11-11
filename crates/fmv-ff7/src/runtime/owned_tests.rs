//! Checks of the runtime against the owner's field data (ignored by default; set `FMV_FF7_DATA` to the game's
//! `data/` directory and run with `--ignored`). Only counts are printed, never game bytes.

use super::*;
use crate::field::{section, FieldFile};
use crate::lgp::LgpArchive;
use crate::story::StoryState;

fn archive() -> Option<LgpArchive<std::fs::File>> {
    let dir = std::env::var_os("FMV_FF7_DATA")?;
    LgpArchive::open_path(std::path::Path::new(&dir).join("field").join("flevel.lgp")).ok()
}

fn fields(lgp: &LgpArchive<std::fs::File>) -> Vec<String> {
    lgp.list().iter().map(|e| e.name.clone()).filter(|n| !n.contains('.') && !n.eq_ignore_ascii_case("maplist")).collect()
}

fn load(lgp: &mut LgpArchive<std::fs::File>, name: &str) -> Option<FieldFile> {
    let stored = lgp.read(name).ok()?;
    FieldFile::parse(crate::lzss::decompress(&stored).ok()?).ok()
}

#[test]
#[ignore = "needs the owner's data (FMV_FF7_DATA)"]
fn the_fresh_reset_story_is_a_complete_image() {
    assert!(Banks::from_story(&StoryState::fresh_reset()).is_some());
    assert!(Banks::from_story(&StoryState::fresh_first_control()).is_none(), "engine-owned bytes are unknown there");
}

/// After init on a fresh story, every NPC the static analysis places (an unconditional placement in the init part)
/// stands where the runtime put that entity's model.
#[test]
#[ignore = "needs the owner's data (FMV_FF7_DATA)"]
fn init_places_the_static_cast_where_the_static_analysis_does() {
    let Some(mut lgp) = archive() else { return };
    let banks = Banks::from_story(&StoryState::fresh_reset()).expect("complete image");
    let (mut compared, mut matched, mut missing_model, mut fields_run) = (0u32, 0u32, 0u32, 0u32);
    let mut mismatched: Vec<String> = Vec::new();
    for name in fields(&lgp) {
        let Some(field) = load(&mut lgp, &name) else { continue };
        let Ok(walkmesh) = field.walkmesh() else { continue };
        let spawns = crate::area_content::npc_spawns(&field, &walkmesh, |_, _| None);
        let Ok(script) = field.section(section::SCRIPT) else { continue };
        let Ok(mut rt) = FieldRuntime::new(script, banks.clone()) else { continue };
        rt.init();
        fields_run += 1;
        for spawn in spawns {
            let named: Vec<usize> = (0..rt.entity_count()).filter(|&e| rt.entity_name(e) == Some(spawn.entity_name.as_str())).collect();
            if named.len() != 1 {
                continue; // an ambiguous name (several entities share it, often the empty one)
            }
            compared += 1;
            let entity = named.first().copied();
            let Some(model) = entity.and_then(|e| rt.models().iter().find(|m| m.entity == e)) else {
                missing_model += 1;
                continue;
            };
            if (model.x as f32, model.y as f32) == (spawn.pos.x, spawn.pos.y) {
                matched += 1;
            } else if mismatched.len() < 12 {
                mismatched.push(format!("{name}/{}", spawn.entity_name));
            }
        }
    }
    eprintln!("fields run {fields_run}; static NPCs {compared}; same place {matched}; no model {missing_model}; e.g. different: {mismatched:?}");
    assert!(fields_run > 600);
    assert!(f64::from(matched) >= 0.99 * f64::from(compared), "{matched}/{compared}");
}

#[test]
fn a_changed_story_byte_is_saved_and_an_engine_owned_one_is_not() {
    let story = StoryState::fresh_reset();
    let banks = Banks::from_story(&story).expect("complete");
    // A tiny field: one entity whose init sets bank 1 address 0x40 and the play-time byte 1:16.
    let init = [0x80, 0x10, 0x40, 0x05, 0x80, 0x10, 16, 0x09, 0x00];
    let section = crate::script::synth::SynthScript { scale: 512, ..Default::default() }.entity("dir", vec![(0, init.to_vec())]).build();
    let mut rt = FieldRuntime::new(&section, banks).expect("runtime");
    rt.init();
    let after = rt.story_after(&story).expect("writes").expect("changed");
    assert_eq!(after.known_byte(1, 0x40), Some(5));
    assert_eq!(after.known_byte(1, 16), story.known_byte(1, 16), "the clock mirror is the engine's, not the story's");
    assert!(Banks::from_story(&after).is_some(), "still a complete image");
    assert_eq!(rt.story_after(&after).expect("writes"), None, "nothing more to save");
}
