//! Re-reads the PMVIE movie name tables from the owned English Steam executable and checks every entry of
//! `fmv_ff7::movies` against them. Reads the file only; nothing of it is kept or printed beyond a mismatch's id and name.
//! Run explicitly with FMV_FF7_EXE pointing at `resources/ff7_1.02/ff7_en` of the owned source.

use fmv_ff7::movies::video_basename;
use std::path::PathBuf;

/// `.data` section of the owned executable: virtual offset, file offset (the table addresses are offsets in this space).
const DATA_VIRTUAL: usize = 0x7BA000;
const DATA_FILE: usize = 3_901_440;
const EXE_BYTES: usize = 5_997_027;
const EXE_BLAKE3: &str = "8465069b87dd8ad685f43b7c249afe42f4088308cba32adbfc674ffb495f5b11";

fn at(virt: usize) -> usize {
    DATA_FILE + (virt - DATA_VIRTUAL)
}

fn name_at(exe: &[u8], slot: usize) -> Option<String> {
    let at_slot = at(slot);
    let pointer = u32::from_le_bytes(exe[at_slot..at_slot + 4].try_into().unwrap()) as usize;
    if pointer == 0 {
        return None;
    }
    let start = at(pointer);
    let end = start + exe[start..].iter().position(|&b| b == 0).unwrap();
    let file = std::str::from_utf8(&exe[start..end]).unwrap();
    Some(file.strip_suffix(".avi").expect("a video name").to_string())
}

#[test]
#[ignore = "needs the owner's FF7 Steam executable (FMV_FF7_EXE)"]
fn every_table_entry_matches_the_owned_executable() {
    let Some(path) = std::env::var_os("FMV_FF7_EXE").map(PathBuf::from) else {
        eprintln!("FMV_FF7_EXE is not set; skipping");
        return;
    };
    let exe = std::fs::read(path).expect("read the executable");
    assert_eq!(exe.len(), EXE_BYTES, "not the inspected executable");
    assert_eq!(blake3::hash(&exe).to_hex().as_str(), EXE_BLAKE3, "not the inspected executable");
    // (disc, first id, slots, table offset)
    let tables: [(u8, u16, usize, usize); 4] = [(1, 0, 20, 0x7BAE80), (1, 20, 34, 0x7BAED0), (2, 20, 42, 0x7BAF58), (3, 20, 10, 0x7BB000)];
    for (disc, first, slots, table) in tables {
        for slot in 0..slots {
            let id = first + slot as u16;
            let native = name_at(&exe, table + 4 * slot);
            assert_eq!(video_basename(disc, id).map(str::to_string), native, "disc {disc} id {id}");
        }
    }
    // Ids 0..=19 are the same on every disc.
    for disc in [2u8, 3] {
        for id in 0..20 {
            assert_eq!(video_basename(disc, id), video_basename(1, id));
        }
    }
}
