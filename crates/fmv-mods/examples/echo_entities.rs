//! Entity names (identifiers only) and MESSAGE/ASK counts per entity of fields, in the original and in Echo-S.
//!
//! ```text
//! FMV_FF7_DATA=<.../data> cargo run --release -p fmv-mods --example echo_entities -- <Echo-s.iro> field...
//! ```
//!
//! Both archives are opened read-only; no text is printed.

use fmv_ff7::field::FieldFile;
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::lzss;
use fmv_ff7::script::{FieldScript, SCRIPTS_PER_ENTITY};
use fmv_mods::IroArchive;

fn section_of(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let raw = lzss::decompress(bytes).map_err(|e| e.to_string())?;
    let file = FieldFile::parse(raw).map_err(|e| e.to_string())?;
    Ok(FieldScript::from_field(&file).map_err(|e| e.to_string())?.bytes().to_vec())
}

fn messages(s: &FieldScript<'_>, entity: usize) -> usize {
    if entity >= s.entity_count() {
        return 0;
    }
    (0..SCRIPTS_PER_ENTITY)
        .map(|slot| s.disassemble_partial(entity, slot).map(|d| d.instructions.iter().filter(|i| matches!(i.opcode, 0x40 | 0x48)).count()).unwrap_or(0))
        .sum()
}

fn main() {
    let mut args = std::env::args().skip(1);
    let iro_path = args.next().expect("usage: echo_entities <Echo-s.iro> field...");
    let data = std::env::var_os("FMV_FF7_DATA").expect("FMV_FF7_DATA");
    let flevel = std::path::Path::new(&data).join("field").join("flevel.lgp");
    let mut orig = LgpArchive::open_path(&flevel).expect("open the original flevel.lgp");
    let echo = IroArchive::open_path(&iro_path).expect("open the Echo-S archive");
    for name in args {
        let ob = orig.read(&name).expect("original field");
        let eb = echo.read_named(&format!("Echo-S\\flevel.lgp\\{name}")).expect("Echo-S field");
        let (os, es) = (section_of(&ob).expect("original section"), section_of(&eb).expect("Echo-S section"));
        let (o, e) = (FieldScript::parse(&os).expect("original script"), FieldScript::parse(&es).expect("Echo-S script"));
        println!("{name}: original {} entities, Echo-S {}", o.entity_count(), e.entity_count());
        for i in 0..o.entity_count().max(e.entity_count()) {
            println!("  {i:>2} {:<10} msgs {:>3} | {:<10} msgs {:>3}", o.entity_name(i).unwrap_or("-"), messages(&o, i), e.entity_name(i).unwrap_or("-"), messages(&e, i));
        }
    }
}
