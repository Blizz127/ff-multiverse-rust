//! Survey of what the owner's real field scripts say about music (needs the owner's data; skips
//! otherwise). Prints ids, offsets, counts and field names only, never game bytes.
//!
//! ```text
//! FMV_FF7_DATA=<.../data> FMV_FF7_AUDIO_DATA=<.../audio data> \
//!     cargo run --release -p fmv-ff7 --example music_survey -- [--evidence|--ambiguity|--tracks]
//! FMV_FF7_DATA=<.../data> cargo run --release -p fmv-ff7 --example music_survey -- --fields
//! FMV_FF7_DATA=<.../data> cargo run --release -p fmv-ff7 --example music_survey -- --ops
//! FMV_FF7_DATA=<.../data> cargo run --release -p fmv-ff7 --example music_survey -- --blocks
//! ```
//!
//! `--evidence` (the default) prints the tables that fix the frame-id rule, `--ambiguity` the
//! measured ambiguity of the extraction policy and where the one-shot fanfares and the id without a
//! track are used, `--tracks` the reverse reading (which field families use each track); `--fields`
//! prints one tab-separated line per field, `--ops` one per music instruction and `--blocks` one per
//! AKAO block (for analysis elsewhere). `FMV_FF7_AUDIO_DATA` (the folder that holds
//! `music/music.idx`) is needed by the tables only.
#[path = "../tests/music_common.rs"]
mod common;

use common::{
    ambiguity_report, audio_dir, data_dir, evidence_report, flevel_path, load_music_list, load_records, size_report, tracks_report, uses_of_frame,
    FieldRecord,
};
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::music::MusicList;

fn ids(v: &[u16]) -> String {
    if v.is_empty() {
        "-".to_string()
    } else {
        v.iter().map(u16::to_string).collect::<Vec<_>>().join(",")
    }
}

fn fields(records: &[FieldRecord]) {
    println!("field\tmaplist\tblocks\tframes\tarrival\tslot0\tmusic\tbattle\tafter\tskipped\takao_ops\tother_ops");
    for r in records {
        let others: Vec<&str> = ["MUSVT", "MUSVM", "MULCK", "CMUSC"].into_iter().filter(|m| r.has_op(m)).collect();
        println!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            r.name,
            r.maplist_index.map_or("-".to_string(), |i| i.to_string()),
            r.blocks.len(),
            r.frames(),
            r.music.arrival().map_or("-".to_string(), |i| i.to_string()),
            ids(&r.slot0_music()),
            ids(&r.music.distinct_music()),
            ids(&r.music.distinct_battle()),
            ids(&r.music.distinct_after_battle()),
            r.music.skipped,
            r.akao_ops,
            if others.is_empty() { "-".to_string() } else { others.join(",") },
        );
    }
}

fn ops(records: &[FieldRecord]) {
    println!("field\tentity\tslot\trefs\toffset\top\toperand\tframe\tphase\treachable\tconditional");
    for r in records {
        for o in &r.ops {
            let frame = o.operand.and_then(|v| usize::try_from(v).ok()).and_then(|b| r.blocks.get(b).copied().flatten());
            println!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                r.name,
                o.entity,
                o.slot,
                o.refs.iter().map(|(e, s)| format!("{e}:{s}")).collect::<Vec<_>>().join(";"),
                o.offset,
                o.mnemonic,
                o.operand.map_or("var".to_string(), |v| v.to_string()),
                frame.map_or("-".to_string(), |f| f.to_string()),
                o.phase.map_or("-".to_string(), |p| format!("{p:?}")),
                u8::from(o.reachable),
                u8::from(o.conditional),
            );
        }
    }
}

fn blocks(records: &[FieldRecord]) {
    println!("field\tindex\tframe\tlen");
    for r in records {
        for (i, b) in r.blocks.iter().enumerate() {
            println!("{}\t{}\t{}\t{}", r.name, i, b.map_or("-".to_string(), |f| f.to_string()), r.block_lens.get(i).copied().unwrap_or(0));
        }
    }
}

fn findings(records: &[FieldRecord], list: &MusicList) {
    print!("{}", ambiguity_report(records, list));
    for track in ["fanfare", "fan2"] {
        let Some(id) = list.frame_id_of(track) else { continue };
        println!("\n== instructions that start `{track}` (frame id {id}); MUSVT and CMUSC included ==");
        for line in uses_of_frame(records, id) {
            println!("  {line}");
        }
    }
    println!("\n== id 1 (no track) ==");
    let one = uses_of_frame(records, 1);
    println!("  {} instructions pick a frame with id 1", one.len());
    let lens: std::collections::BTreeMap<usize, usize> = records.iter().flat_map(|r| r.blocks.iter().zip(&r.block_lens)).filter(|(b, _)| **b == Some(1)).fold(Default::default(), |mut m, (_, &l)| {
        *m.entry(l).or_default() += 1;
        m
    });
    println!("  block lengths of the frames with id 1 (length: blocks): {lens:?}");
    println!("\n== maplist order md1stin (116) .. elevtr1 (121) ==");
    for r in records.iter().filter(|r| r.maplist_index.is_some_and(|i| (116..=125).contains(&i))) {
        let name = |id: u16| list.track_for_frame_id(id).unwrap_or("-").to_string();
        println!(
            "  {:>3} {:<9} arrival {:<9} music {:?} battle {:?} after {:?}",
            r.maplist_index.unwrap_or(0),
            r.name,
            r.music.arrival().map_or("-".to_string(), name),
            r.music.distinct_music().into_iter().map(name).collect::<Vec<_>>(),
            r.music.distinct_battle().into_iter().map(name).collect::<Vec<_>>(),
            r.music.distinct_after_battle().into_iter().map(name).collect::<Vec<_>>(),
        );
    }
}

fn main() {
    let Some(dir) = data_dir() else {
        eprintln!("FMV_FF7_DATA is not set; skipping (point it at the game's data/ directory)");
        return;
    };
    let mode = std::env::args().nth(1).unwrap_or_else(|| "--evidence".to_string());
    let path = flevel_path(&dir);
    let mut lgp = match LgpArchive::open_path(&path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("cannot open {}: {e}", path.display());
            return;
        }
    };
    let records = match load_records(&mut lgp) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("cannot read the fields: {e}");
            return;
        }
    };
    match mode.as_str() {
        "--fields" => fields(&records),
        "--ops" => ops(&records),
        "--blocks" => blocks(&records),
        "--evidence" | "--ambiguity" | "--tracks" => {
            let Some(audio) = audio_dir() else {
                eprintln!("FMV_FF7_AUDIO_DATA is not set; skipping (point it at the audio data/ directory)");
                return;
            };
            let list = match load_music_list(&audio) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("cannot read music.idx: {e}");
                    return;
                }
            };
            match mode.as_str() {
                "--evidence" => {
                    print!("{}", evidence_report(&records, &list));
                    println!();
                    print!("{}", size_report(&records, &list, &audio));
                }
                "--tracks" => print!("{}", tracks_report(&records, &list)),
                _ => findings(&records, &list),
            }
        }
        other => eprintln!("unknown option {other}; use --evidence, --ambiguity, --tracks, --fields, --ops or --blocks"),
    }
}
