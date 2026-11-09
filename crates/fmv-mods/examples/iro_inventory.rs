//! Prints an inventory of a `.iro` archive: counts, folders and extensions, never a byte of any file.
//!
//! ```text
//! cargo run --offline -p fmv-mods --example iro_inventory -- <archive.iro> [--list]
//! ```
//!
//! `--list` also prints every entry's compression nibble, size and name (paths only). The archive is opened
//! read-only; nothing is extracted. Use it to check that a download is complete on a machine: a truncated
//! archive is refused with the name of the first entry that runs past the end of the file.

use fmv_mods::IroArchive;
use std::collections::BTreeMap;

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: iro_inventory <archive.iro> [--list]");
        std::process::exit(2);
    };
    let list = args.any(|a| a == "--list");
    let archive = match IroArchive::open_path(&path) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("cannot read the archive: {e}");
            std::process::exit(1);
        }
    };
    println!("version        : {:#x}", archive.version());
    println!("archive flags  : {:#x}{}", archive.archive_flags(), if archive.is_patch() { " (a patch archive)" } else { "" });
    println!("file bytes     : {}", archive.file_len());
    println!("entries        : {}", archive.len());
    println!("duplicate names: {}", archive.duplicate_names());
    let mut compression: BTreeMap<String, usize> = BTreeMap::new();
    let mut top: BTreeMap<String, (usize, u64)> = BTreeMap::new();
    let mut depth2: BTreeMap<String, usize> = BTreeMap::new();
    let mut ext: BTreeMap<String, usize> = BTreeMap::new();
    let mut max_end = 0u64;
    for e in archive.entries() {
        *compression.entry(e.compression().label()).or_default() += 1;
        let parts: Vec<&str> = e.name.split('\\').collect();
        let first = if parts.len() > 1 { parts.first().copied().unwrap_or("") } else { "<root>" };
        let slot = top.entry(first.to_string()).or_default();
        slot.0 += 1;
        slot.1 += u64::from(e.length);
        if parts.len() > 2 {
            *depth2.entry(parts.iter().take(2).copied().collect::<Vec<_>>().join("\\")).or_default() += 1;
        }
        let extension = e.file_name().rsplit_once('.').map_or("<none>".to_string(), |(_, x)| x.to_ascii_lowercase());
        *ext.entry(extension).or_default() += 1;
        max_end = max_end.max(e.offset + u64::from(e.length));
    }
    println!("last entry ends: {max_end} (slack to the end of the file: {})", archive.file_len().saturating_sub(max_end));
    println!("compression    : {compression:?}");
    println!("top-level folders (entries, stored bytes):");
    for (name, (n, bytes)) in &top {
        println!("  {n:7}  {bytes:>14}  {name}");
    }
    println!("two-level folders:");
    for (name, n) in &depth2 {
        println!("  {n:7}  {name}");
    }
    println!("extensions:");
    for (name, n) in &ext {
        println!("  {n:7}  {name}");
    }
    if list {
        for e in archive.entries() {
            println!("{} {:>10} {}", e.flags & 0xF, e.length, e.name);
        }
    }
}
