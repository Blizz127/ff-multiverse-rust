//! Print one owned field's walkmesh as JSON for test bots (needs the owner's data): every triangle's three vertices and
//! three access links. Numbers only; write it to a scratch directory, never into the repository (it is game data).
//!
//! ```text
//! FMV_FF7_DATA=<.../data> cargo run -p fmv-ff7 --example walkmesh_json -- FIELD > /scratch/FIELD.walk.json
//! ```
#[path = "../tests/script_common.rs"]
mod common;

use common::{data_dir, flevel_path, load_field};
use fmv_ff7::lgp::LgpArchive;

fn main() {
    let Some(dir) = data_dir() else {
        eprintln!("FMV_FF7_DATA is not set; skipping");
        return;
    };
    let Some(name) = std::env::args().nth(1) else {
        eprintln!("usage: walkmesh_json FIELD");
        return;
    };
    let mut lgp = LgpArchive::open_path(&flevel_path(&dir)).expect("flevel");
    let field = load_field(&mut lgp, &name).expect("field");
    let walk = field.walkmesh().expect("walkmesh");
    let tris: Vec<String> = walk
        .triangles
        .iter()
        .zip(&walk.access)
        .map(|(t, a)| {
            format!(
                "{{\"v\":[[{},{},{}],[{},{},{}],[{},{},{}]],\"a\":[{},{},{}]}}",
                t[0].x, t[0].y, t[0].z, t[1].x, t[1].y, t[1].z, t[2].x, t[2].y, t[2].z,
                if a[0] == 0xFFFF { -1 } else { i32::from(a[0]) },
                if a[1] == 0xFFFF { -1 } else { i32::from(a[1]) },
                if a[2] == 0xFFFF { -1 } else { i32::from(a[2]) }
            )
        })
        .collect();
    let gates: Vec<String> = field
        .triggers()
        .map(|t| {
            t.gateways
                .iter()
                .map(|g| format!("{{\"a\":[{},{}],\"b\":[{},{}],\"to\":{}}}", g.exit_a[0], g.exit_a[1], g.exit_b[0], g.exit_b[1], g.target_field_id))
                .collect()
        })
        .unwrap_or_default();
    println!("{{\"field\":\"{name}\",\"triangles\":[{}],\"gates\":[{}]}}", tris.join(","), gates.join(","));
}
