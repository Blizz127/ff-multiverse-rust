//! The characters of a field as a scene's cast names them (needs the owner's data; skips otherwise).
//!
//! ```text
//! FMV_FF7_DATA=<.../data> cargo run -p fmv-ff7 --example cast_stems -- md1_1 [nmkin_1 ...]
//! ```
//!
//! For every entity of the field that has a model (`CHAR`), prints its index and name, the model's `char.lgp` skeleton stem
//! and the stem of its first animation (what `fmv_ff7::script::scene::Look::Model` takes), and whether it is bound to a
//! playable character (`PC`). Numbers and names only, never game text; writes nothing.
#[path = "../tests/script_common.rs"]
mod common;

use common::{data_dir, flevel_path, load_field};
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::script::{parse_model_list, FieldScript};

fn main() {
    let Some(dir) = data_dir() else {
        eprintln!("FMV_FF7_DATA is not set; skipping (point it at the game's data/ directory)");
        return;
    };
    let mut lgp = LgpArchive::open_path(flevel_path(&dir)).expect("flevel.lgp");
    for name in std::env::args().skip(1) {
        let field = match load_field(&mut lgp, &name) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("{name}: {e}");
                continue;
            }
        };
        let script = FieldScript::from_field(&field).expect("a script");
        let loader = field.model_loader().expect("a model loader");
        let models = parse_model_list(field.section(fmv_ff7::field::section::MODEL_LOADER).expect("the loader section")).expect("a model list");
        println!("=== {name}");
        for init in script.entity_inits(Some(&models)) {
            let Some(index) = init.model_index else { continue };
            let Some(model) = loader.models.get(index) else { continue };
            let idle = model.animations.first().map(|a| a.stem()).unwrap_or_default();
            println!(
                "  entity {:>2} {:<10} model #{index} hrc {} idle {}{}",
                init.entity,
                init.name,
                model.hrc_stem(),
                idle,
                if init.is_player() { " (bound to a playable character)" } else { "" }
            );
        }
    }
}
