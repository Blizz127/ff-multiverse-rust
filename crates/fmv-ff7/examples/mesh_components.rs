//! Which parts of a field's floor can be walked between (needs the owner's data; skips otherwise).
//!
//! ```text
//! FMV_FF7_DATA=<.../data> cargo run -p fmv-ff7 --example mesh_components -- md8_1 [x,y ...]
//! ```
//!
//! Prints the walkmesh's connected parts (triangles joined by an edge that can be crossed), the triangle and part that
//! hold each gateway line's middle, and the triangle and part of each `x,y` given. A place a script puts somebody (a
//! `MAPJUMP` arrival, a witness spot) must be in the part the field's ways out are in, or whoever is put there is
//! stranded: a script's `JUMP` crosses parts, a player's walk does not. Numbers only, never game text; writes nothing.
#[path = "../tests/script_common.rs"]
mod common;

use common::{data_dir, flevel_path, load_field};
use fmv_ff7::field::Walkmesh;
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::script::triangle_contains_xy;

/// The part (0, 1, ...) of every triangle: the parts are numbered by their lowest triangle.
fn parts(mesh: &Walkmesh) -> Vec<usize> {
    let n = mesh.triangles.len();
    let mut part = vec![usize::MAX; n];
    let mut next = 0;
    for start in 0..n {
        if part[start] != usize::MAX {
            continue;
        }
        let mut stack = vec![start];
        part[start] = next;
        while let Some(t) = stack.pop() {
            for edge in 0..3 {
                // An edge is crossed one way or the other: either triangle may name its neighbour.
                if let Some(other) = mesh.neighbor(t, edge) {
                    if part[other] == usize::MAX {
                        part[other] = next;
                        stack.push(other);
                    }
                }
            }
            for (other, slot) in part.iter_mut().enumerate() {
                if *slot == usize::MAX && (0..3).any(|edge| mesh.neighbor(other, edge) == Some(t)) {
                    *slot = next;
                    stack.push(other);
                }
            }
        }
        next += 1;
    }
    part
}

fn triangle_at(mesh: &Walkmesh, x: i32, y: i32) -> Vec<usize> {
    mesh.triangles.iter().enumerate().filter(|(_, t)| triangle_contains_xy(t, x, y)).map(|(i, _)| i).collect()
}

fn main() {
    let Some(dir) = data_dir() else {
        eprintln!("FMV_FF7_DATA is not set; skipping (point it at the game's data/ directory)");
        return;
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(name) = args.first() else {
        eprintln!("usage: mesh_components <field> [x,y ...]");
        return;
    };
    let mut lgp = LgpArchive::open_path(flevel_path(&dir)).expect("flevel.lgp");
    let field = load_field(&mut lgp, name).expect("the field");
    let mesh = field.walkmesh().expect("a walkmesh");
    let part = parts(&mesh);
    let count = part.iter().copied().max().map_or(0, |m| m + 1);
    println!("{name}: {} triangles in {count} part(s)", mesh.triangles.len());
    for p in 0..count {
        let members: Vec<usize> = (0..part.len()).filter(|&t| part[t] == p).collect();
        let (mut min, mut max) = ([i32::MAX; 2], [i32::MIN; 2]);
        for &t in &members {
            for v in &mesh.triangles[t] {
                min = [min[0].min(i32::from(v.x)), min[1].min(i32::from(v.y))];
                max = [max[0].max(i32::from(v.x)), max[1].max(i32::from(v.y))];
            }
        }
        println!("  part {p}: {} triangle(s) {members:?} x {}..{} y {}..{}", members.len(), min[0], max[0], min[1], max[1]);
    }
    let describe = |label: String, x: i32, y: i32| {
        let on = triangle_at(&mesh, x, y);
        let where_: Vec<String> = on.iter().map(|&t| format!("triangle {t} (part {})", part[t])).collect();
        println!("  {label} ({x}, {y}): {}", if where_.is_empty() { "on no triangle".to_string() } else { where_.join(", ") });
    };
    if let Ok(triggers) = field.triggers() {
        for (slot, g) in triggers.gateways.iter().enumerate().filter(|(_, g)| g.is_used()) {
            let (a, b) = (g.exit_a, g.exit_b);
            let (mx, my) = ((i32::from(a[0]) + i32::from(b[0])) / 2, (i32::from(a[1]) + i32::from(b[1])) / 2);
            describe(format!("gateway {slot} to field {} line middle", g.target_field_id), mx, my);
        }
    }
    for arg in args.iter().skip(1) {
        let mut it = arg.split(',').map(|v| v.trim().parse::<i32>());
        if let (Some(Ok(x)), Some(Ok(y))) = (it.next(), it.next()) {
            describe("point".to_string(), x, y);
        } else {
            eprintln!("not x,y: {arg}");
        }
    }
}
