//! Shared survey code for `tests/music_real.rs` and `examples/music_survey.rs`.
//!
//! Reads the owner's `flevel.lgp` and `music/music.idx` (read-only) and records ids, offsets,
//! counts and field names only: no game bytes are kept or printed. The evidence that fixes the
//! frame-id rule is computed here (the result is in the module docs of `fmv_ff7::music`):
//!
//! * [`REGIONS`] is the expectation table. It was written down **before** any id was scored and
//!   it uses nothing but names: a region is a group of field names (`rktinn1`, `rckt3`, ...) and
//!   the track whose own name says the same thing (`rocket`). No id and no line number of
//!   `music.idx` enters the table.
//! * [`score_k`] maps every field's music id to a track under the rule "1-based line = id - k"
//!   and counts the member fields that land on an allowed track. Every k uses the same
//!   denominator (all member fields): a field without that music, or an id that has no line under
//!   that k, is a miss for every k alike.
//! * [`offset_ranking`] widens the search from four candidate k to every offset in -96..=96.
#![allow(dead_code)]

use fmv_ff7::field::FieldFile;
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::lzss;
use fmv_ff7::music::{field_music, FieldMusic, MusicList, MusicUse};
use fmv_ff7::script::{Disassembly, FieldScript, Maplist, Phase, INIT_SCRIPT, SCRIPTS_PER_ENTITY};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs::File;
use std::path::{Path, PathBuf};

pub fn data_dir() -> Option<PathBuf> {
    std::env::var_os("FMV_FF7_DATA").map(PathBuf::from)
}

pub fn audio_dir() -> Option<PathBuf> {
    std::env::var_os("FMV_FF7_AUDIO_DATA").map(PathBuf::from)
}

pub fn flevel_path(dir: &Path) -> PathBuf {
    dir.join("field").join("flevel.lgp")
}

pub fn music_idx_path(audio: &Path) -> PathBuf {
    audio.join("music").join("music.idx")
}

/// Whether the audio folder holds the track: `music_ogg/<name>.ogg` or `music/<name>.wav`.
pub fn track_file_exists(audio: &Path, name: &str) -> bool {
    audio.join("music_ogg").join(format!("{name}.ogg")).is_file() || audio.join("music").join(format!("{name}.wav")).is_file()
}

/// Field entries are the ones whose names have no '.', excluding the name list.
pub fn is_field_name(name: &str) -> bool {
    !name.contains('.') && !name.eq_ignore_ascii_case("maplist")
}

pub fn load_music_list(audio: &Path) -> Result<MusicList, String> {
    let bytes = std::fs::read(music_idx_path(audio)).map_err(|e| e.to_string())?;
    Ok(MusicList::parse(&bytes))
}

/// Opcodes that touch music: `(opcode, mnemonic, operand name)`. `MUSIC`, `BMUSC` and `FMUSC` are
/// what `fmv_ff7::music` extracts; the others are scanned to measure what it leaves out.
pub const MUSIC_OPS: [(u8, &str, &str); 7] = [
    (0xF0, "MUSIC", "id"),
    (0xF3, "MUSVT", "id"),
    (0xF4, "MUSVM", "id"),
    (0xF5, "MULCK", "disabled"),
    (0xF6, "BMUSC", "id"),
    (0xFC, "FMUSC", "id"),
    (0xFD, "CMUSC", "music"),
];
/// `AKAO` and `AKAO2`: the generic sound opcodes (counted per field, not decoded).
pub const AKAO_OPS: [u8; 2] = [0xF2, 0xDA];

/// One music-related instruction found by [`scan_music_ops`].
#[derive(Debug, Clone)]
pub struct RawOp {
    pub opcode: u8,
    pub mnemonic: &'static str,
    /// The literal operand (`None` when it names a variable).
    pub operand: Option<i32>,
    /// The first slot that points at the script holding the instruction.
    pub entity: usize,
    pub slot: usize,
    /// Every `(entity, slot)` that points at that script.
    pub refs: Vec<(usize, usize)>,
    pub offset: usize,
    /// Slot 0 only: the part of script 0 the instruction lies in.
    pub phase: Option<Phase>,
    /// Reachable from the script's entry points (script 0 has two: the start and the main part).
    pub reachable: bool,
    /// Lies in a region that a reachable forward jump skips on some path (the crate's script-0
    /// analysis rule; `fmv_ff7::music` uses every forward jump, reachable or not).
    pub conditional: bool,
}

/// Reachable offsets and the `[from, to)` regions skipped by forward jumps (the same rules as the
/// crate's own script-0 analysis, which keeps them private).
fn flow(d: &Disassembly, slot0: bool) -> (BTreeSet<usize>, Vec<(usize, usize)>) {
    let index: BTreeMap<usize, usize> = d.instructions.iter().enumerate().map(|(i, ins)| (ins.offset, i)).collect();
    let first_ret = d.instructions.iter().position(|i| i.opcode == 0x00);
    let main_start = if slot0 { first_ret.and_then(|i| d.instructions.get(i + 1)).map(|i| i.offset) } else { None };
    let mut reachable = BTreeSet::new();
    let mut stack: Vec<usize> = d.instructions.first().map(|i| i.offset).into_iter().chain(main_start).collect();
    while let Some(off) = stack.pop() {
        if !reachable.insert(off) {
            continue;
        }
        let Some(ins) = index.get(&off).and_then(|&i| d.instructions.get(i)) else { continue };
        if !ins.ends_flow() && index.contains_key(&ins.end()) {
            stack.push(ins.end());
        }
        if let Some(t) = ins.jump_target.filter(|t| index.contains_key(t)) {
            stack.push(t);
        }
    }
    let skipped = d
        .instructions
        .iter()
        .filter(|ins| reachable.contains(&ins.offset) && ins.is_jump())
        .filter_map(|ins| ins.jump_target.filter(|&t| t > ins.offset).map(|t| (ins.end(), t)))
        .collect();
    (reachable, skipped)
}

/// Every music-related instruction of every distinct script of a field, with the slots that share
/// the script and its reachability. Scripts come in order of their entry offset.
pub fn scan_music_ops(script: &FieldScript<'_>) -> Vec<RawOp> {
    let mut refs: BTreeMap<usize, Vec<(usize, usize)>> = BTreeMap::new();
    for entity in 0..script.entity_count() {
        for slot in 0..SCRIPTS_PER_ENTITY {
            if let Some(span) = script.span(entity, slot) {
                refs.entry(span.start).or_default().push((entity, slot));
            }
        }
    }
    let mut out = Vec::new();
    for who in refs.values() {
        let Some(&(entity, slot)) = who.first() else { continue };
        let Ok(d) = script.disassemble_partial(entity, slot) else { continue };
        let slot0 = slot == INIT_SCRIPT;
        let (reachable, skipped) = flow(&d, slot0);
        let first_ret = d.instructions.iter().find(|i| i.opcode == 0x00).map(|i| i.offset);
        for ins in &d.instructions {
            let Some(&(opcode, mnemonic, operand)) = MUSIC_OPS.iter().find(|(op, _, _)| *op == ins.opcode) else { continue };
            out.push(RawOp {
                opcode,
                mnemonic,
                operand: ins.literal(operand),
                entity,
                slot,
                refs: who.clone(),
                offset: ins.offset,
                phase: slot0.then_some(match first_ret {
                    Some(r) if ins.offset > r => Phase::Main,
                    _ => Phase::Init,
                }),
                reachable: reachable.contains(&ins.offset),
                conditional: skipped.iter().any(|&(from, to)| from <= ins.offset && ins.offset < to),
            });
        }
    }
    out
}

/// Number of `AKAO`/`AKAO2` instructions in the distinct scripts of a field.
fn count_akao_ops(script: &FieldScript<'_>) -> usize {
    let mut seen = BTreeSet::new();
    let mut n = 0;
    for entity in 0..script.entity_count() {
        for slot in 0..SCRIPTS_PER_ENTITY {
            let Some(span) = script.span(entity, slot) else { continue };
            if !seen.insert(span.start) {
                continue;
            }
            if let Ok(d) = script.disassemble_partial(entity, slot) {
                n += d.instructions.iter().filter(|i| AKAO_OPS.contains(&i.opcode)).count();
            }
        }
    }
    n
}

/// What the survey keeps of one field.
#[derive(Debug, Clone)]
pub struct FieldRecord {
    /// The archive entry name (lower case in the owned data).
    pub name: String,
    /// Position in the `maplist` (the order the game numbers its fields in).
    pub maplist_index: Option<usize>,
    /// Per AKAO block, in block order: the frame id (`None` for a block that is not a real frame).
    pub blocks: Vec<Option<u16>>,
    /// Per AKAO block, in block order: its length in bytes.
    pub block_lens: Vec<usize>,
    pub music: FieldMusic,
    pub ops: Vec<RawOp>,
    /// `AKAO`/`AKAO2` instructions in the field.
    pub akao_ops: usize,
}

impl FieldRecord {
    /// Real AKAO frames of the field.
    pub fn frames(&self) -> usize {
        self.blocks.iter().flatten().count()
    }

    /// Distinct frame ids of the `MUSIC` uses in slot 0 (the entities' own scripts), in script order.
    pub fn slot0_music(&self) -> Vec<u16> {
        distinct(self.music.music.iter().filter(|u| u.slot == INIT_SCRIPT))
    }

    /// The slot-0 `MUSIC` uses in script order.
    pub fn slot0_uses(&self) -> Vec<&MusicUse> {
        self.music.music.iter().filter(|u| u.slot == INIT_SCRIPT).collect()
    }

    /// Whether the field runs an opcode called `mnemonic` anywhere.
    pub fn has_op(&self, mnemonic: &str) -> bool {
        self.ops.iter().any(|o| o.mnemonic == mnemonic)
    }

    /// The distinct frame ids of all real AKAO blocks, in block order.
    pub fn frame_ids(&self) -> Vec<u16> {
        let mut seen = BTreeSet::new();
        self.blocks.iter().flatten().copied().filter(|id| seen.insert(*id)).collect()
    }
}

pub fn distinct<'a>(uses: impl Iterator<Item = &'a MusicUse>) -> Vec<u16> {
    let mut seen = BTreeSet::new();
    uses.map(|u| u.frame_id).filter(|id| seen.insert(*id)).collect()
}

/// Walk every field of `lgp`, in archive order.
pub fn load_records(lgp: &mut LgpArchive<File>) -> Result<Vec<FieldRecord>, String> {
    let maplist = Maplist::from_archive(lgp).map_err(|e| e.to_string())?;
    let entries: Vec<(usize, String)> =
        lgp.list().iter().enumerate().filter(|(_, e)| is_field_name(&e.name)).map(|(i, e)| (i, e.name.clone())).collect();
    let mut out = Vec::with_capacity(entries.len());
    for (index, name) in entries {
        let stored = lgp.read_index(index).map_err(|e| format!("{name}: {e}"))?;
        let bytes = lzss::decompress(&stored).map_err(|e| format!("{name}: {e}"))?;
        let field = FieldFile::parse(bytes).map_err(|e| format!("{name}: {e}"))?;
        let script = FieldScript::from_field(&field).map_err(|e| format!("{name}: {e}"))?;
        let data = script.bytes();
        let count = script.akao_blocks().iter().map(|b| b.index + 1).max().unwrap_or(0);
        let mut blocks: Vec<Option<u16>> = vec![None; count];
        let mut block_lens = vec![0usize; count];
        for b in script.akao_blocks() {
            if let Some(len) = block_lens.get_mut(b.index) {
                *len = b.len;
            }
            if !b.is_akao() || b.len < 6 {
                continue;
            }
            let at = b.offset as usize + 4;
            let id = data.get(at..at + 2).map(|s| u16::from_le_bytes([s[0], s[1]]));
            if let Some(slot) = blocks.get_mut(b.index) {
                *slot = id;
            }
        }
        out.push(FieldRecord {
            maplist_index: maplist.id_of(&name).map(usize::from),
            music: field_music(&script),
            ops: scan_music_ops(&script),
            akao_ops: count_akao_ops(&script),
            blocks,
            block_lens,
            name,
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------
// The evidence
// ---------------------------------------------------------------------------------------

/// The offsets scored: the track of an id is line `id - k` of `music.idx`, counted from 1.
pub const KS: [i32; 4] = [-1, 0, 1, 2];

/// The track an id names under the rule "1-based line = id - k" (`None` when no such line).
pub fn track_under_k(list: &MusicList, id: u16, k: i32) -> Option<&str> {
    let index = usize::try_from(i32::from(id) - k - 1).ok()?;
    list.name_at(index)
}

/// A group of fields whose names say what music they should have, and the tracks whose names say
/// the same. Field patterns: `name` is exact, `prefix*` a prefix, `*part*` a substring.
pub struct Region {
    pub label: &'static str,
    pub fields: &'static [&'static str],
    /// A field is a hit when its music is one of these tracks.
    pub tracks: &'static [&'static str],
    /// Score every `MUSIC` id of the field (any slot) instead of the arrival id: for a jingle that
    /// is played from a talk script, not at arrival.
    pub any_use: bool,
}

/// The expectation table (see the module docs: names only, written before scoring).
pub const REGIONS: &[Region] = &[
    Region { label: "Shinra HQ (blin*, sinbil_*)", fields: &["blin*", "sinbil_*"], tracks: &["sinra", "sinraslo"], any_use: false },
    Region { label: "Junon streets", fields: &["junon", "junonl*", "junonr*", "ujunon*", "ujun_w"], tracks: &["junon"], any_use: false },
    Region { label: "Gold Saucer plaza", fields: &["gldst", "gldgate", "gldinfo", "gldelev"], tracks: &["gold1"], any_use: false },
    Region { label: "Corel and North Corel", fields: &["corel1", "corel2", "corel3", "corelin", "ncorel*", "ncoin*"], tracks: &["corel"], any_use: false },
    Region { label: "Rocket Town", fields: &["rckt", "rckt2", "rckt3", "rkt_w", "rkt_i", "rktinn*", "rktsid"], tracks: &["rocket"], any_use: false },
    Region { label: "Fort Condor", fields: &["condor1", "condor2"], tracks: &["condor"], any_use: false },
    Region { label: "Icicle Inn (snow, snmin*, sninn_*)", fields: &["snow", "snmin*", "sninn_*", "snw_w", "snmayor"], tracks: &["snow"], any_use: false },
    Region { label: "Don Corneo's mansion (colne_*)", fields: &["colne_*"], tracks: &["corneo"], any_use: false },
    Region { label: "Cosmo Canyon", fields: &["cosmo", "cosmo2", "cos_btm", "cos_btm2", "cos_top"], tracks: &["canyon"], any_use: false },
    Region { label: "Costa del Sol (del*)", fields: &["del1", "del12", "del2", "del3", "delmin*", "delinn", "delpb"], tracks: &["costa"], any_use: false },
    Region { label: "Wutai", fields: &["uta_wa", "uta_im", "utmin*", "uutai*", "utapb"], tracks: &["utai"], any_use: false },
    Region { label: "Chocobo farm", fields: &["farm", "frmin", "frcyo"], tracks: &["bokujo"], any_use: false },
    Region { label: "Inn rooms (*inn*), any MUSIC use", fields: &["*inn*"], tracks: &["yado"], any_use: true },
];

impl Region {
    /// A region with one allowed track separates the candidate offsets; a region with two
    /// adjacent tracks (`sinra`, `sinraslo`) cannot tell +1 from +2.
    pub fn is_decisive(&self) -> bool {
        self.tracks.len() == 1
    }
}

fn pattern_matches(pattern: &str, name: &str) -> bool {
    if let Some(mid) = pattern.strip_prefix('*').and_then(|p| p.strip_suffix('*')) {
        name.contains(mid)
    } else if let Some(prefix) = pattern.strip_suffix('*') {
        name.starts_with(prefix)
    } else {
        name == pattern
    }
}

pub fn region_members<'a>(records: &'a [FieldRecord], region: &Region) -> Vec<&'a FieldRecord> {
    records.iter().filter(|r| region.fields.iter().any(|p| pattern_matches(p, &r.name))).collect()
}

/// Which of a field's ids are scored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The arrival id only (the first `MUSIC` in slot 0).
    Arrival,
    /// Every `MUSIC` id of the field, any slot.
    AnyMusic,
    /// Every frame id the field carries, used or not.
    AnyFrame,
}

fn ids_in(r: &FieldRecord, scope: Scope) -> Vec<u16> {
    match scope {
        Scope::Arrival => r.music.arrival().into_iter().collect(),
        Scope::AnyMusic => r.music.distinct_music(),
        Scope::AnyFrame => r.frame_ids(),
    }
}

/// Member fields of `region` that land on an allowed track under `k`, of all member fields. The
/// jingle region scores every `MUSIC` use even for [`Scope::Arrival`].
pub fn region_hits(records: &[FieldRecord], list: &MusicList, region: &Region, k: i32, scope: Scope) -> (usize, usize) {
    let scope = if region.any_use && scope == Scope::Arrival { Scope::AnyMusic } else { scope };
    let members = region_members(records, region);
    let hits = members
        .iter()
        .filter(|r| ids_in(r, scope).into_iter().any(|id| track_under_k(list, id, k).is_some_and(|t| region.tracks.contains(&t))))
        .count();
    (hits, members.len())
}

/// The evidence for one `k`.
#[derive(Debug, Clone)]
pub struct KScore {
    pub k: i32,
    /// `(hits, member fields)` per region of [`REGIONS`], scoring the arrival id.
    pub arrival: Vec<(usize, usize)>,
    /// The same, scoring every `MUSIC` id of the field.
    pub any_music: Vec<(usize, usize)>,
    /// Distinct (field, BMUSC operand) uses, and how many name the track `bat` under `k`.
    pub battle_uses: usize,
    pub battle_on_bat: usize,
    /// Distinct arrival ids that have no line under `k`, of all distinct arrival ids.
    pub arrival_ids_without_track: usize,
    pub arrival_ids: usize,
}

fn total(rows: &[(usize, usize)], keep: impl Fn(usize) -> bool) -> (usize, usize) {
    rows.iter().enumerate().filter(|(i, _)| keep(*i)).fold((0, 0), |(h, m), (_, r)| (h + r.0, m + r.1))
}

impl KScore {
    /// `(hits, members)` over all regions, scoring the arrival id.
    pub fn arrival_total(&self) -> (usize, usize) {
        total(&self.arrival, |_| true)
    }

    /// `(hits, members)` over the regions with one allowed track, scoring the arrival id.
    pub fn arrival_decisive(&self) -> (usize, usize) {
        total(&self.arrival, |i| REGIONS.get(i).is_some_and(Region::is_decisive))
    }

    /// `(hits, members)` over all regions, scoring every `MUSIC` id.
    pub fn any_music_total(&self) -> (usize, usize) {
        total(&self.any_music, |_| true)
    }

    /// `(hits, members)` over the regions with one allowed track, scoring every `MUSIC` id.
    pub fn any_music_decisive(&self) -> (usize, usize) {
        total(&self.any_music, |i| REGIONS.get(i).is_some_and(Region::is_decisive))
    }
}

pub fn score_k(records: &[FieldRecord], list: &MusicList, k: i32) -> KScore {
    let uses = battle_uses(records);
    let arrival_ids: BTreeSet<u16> = records.iter().filter_map(|r| r.music.arrival()).collect();
    KScore {
        k,
        arrival: REGIONS.iter().map(|r| region_hits(records, list, r, k, Scope::Arrival)).collect(),
        any_music: REGIONS.iter().map(|r| region_hits(records, list, r, k, Scope::AnyMusic)).collect(),
        battle_uses: uses.len(),
        battle_on_bat: uses.values().filter(|&&id| track_under_k(list, id, k) == Some("bat")).count(),
        arrival_ids_without_track: arrival_ids.iter().filter(|&&id| track_under_k(list, id, k).is_none()).count(),
        arrival_ids: arrival_ids.len(),
    }
}

/// The distinct (field, `BMUSC` operand) uses with the frame id each picks.
pub fn battle_uses(records: &[FieldRecord]) -> BTreeMap<(String, u8), u16> {
    let mut uses = BTreeMap::new();
    for r in records {
        for u in &r.music.battle {
            uses.insert((r.name.clone(), u.block), u.frame_id);
        }
    }
    uses
}

/// Votes for every offset `d` in `-96..=96`: the number of member fields (of the regions with one
/// allowed track) that have an id equal to `line(track) + d`. The chosen rule is `d = +1`.
/// Returns the votes by offset (zero entries left out) and the number of member fields.
pub fn offset_ranking(records: &[FieldRecord], list: &MusicList, scope: Scope) -> (BTreeMap<i32, usize>, usize) {
    let mut votes: BTreeMap<i32, usize> = BTreeMap::new();
    let mut members = 0;
    for region in REGIONS.iter().filter(|r| r.is_decisive() && !(r.any_use && scope == Scope::Arrival)) {
        let lines: Vec<i32> = region.tracks.iter().filter_map(|t| list.index_of(t)).filter_map(|i| i32::try_from(i + 1).ok()).collect();
        for r in region_members(records, region) {
            members += 1;
            let offsets: BTreeSet<i32> =
                ids_in(r, scope).into_iter().flat_map(|id| lines.iter().map(move |l| i32::from(id) - l)).filter(|d| (-96..=96).contains(d)).collect();
            for d in offsets {
                *votes.entry(d).or_default() += 1;
            }
        }
    }
    (votes, members)
}

/// The votes sorted by descending count (offset ascending on ties).
pub fn ranked(votes: &BTreeMap<i32, usize>) -> Vec<(i32, usize)> {
    let mut v: Vec<(i32, usize)> = votes.iter().map(|(&d, &n)| (d, n)).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v
}

/// The text of the evidence tables (what `--nocapture` and the example print).
pub fn evidence_report(records: &[FieldRecord], list: &MusicList) -> String {
    let mut o = String::new();
    let scores: Vec<KScore> = KS.iter().map(|&k| score_k(records, list, k)).collect();
    for (title, pick) in [("A: the arrival id (first MUSIC in slot 0)", 0), ("B: any MUSIC id of the field (any slot)", 1)] {
        let _ = writeln!(o, "== Evidence {title}; member fields on an allowed track under \"1-based line = id - k\" (k = +1 is the chosen rule) ==");
        let _ = write!(o, "{:<58} {:>6}", "region [allowed tracks]", "fields");
        for k in KS {
            let _ = write!(o, " {:>6}", format!("k={k:+}"));
        }
        let _ = writeln!(o);
        for (i, region) in REGIONS.iter().enumerate() {
            let row = |s: &KScore| if pick == 0 { s.arrival.get(i).copied() } else { s.any_music.get(i).copied() }.unwrap_or((0, 0));
            let _ = write!(o, "{:<58} {:>6}", format!("{} [{}]{}", region.label, region.tracks.join("/"), if region.is_decisive() { "" } else { " *" }), scores.first().map_or(0, |s| row(s).1));
            for s in &scores {
                let _ = write!(o, " {:>6}", row(s).0);
            }
            let _ = writeln!(o);
        }
        for (label, decisive) in [("TOTAL, all regions", false), ("TOTAL, regions with one allowed track", true)] {
            let tot = |s: &KScore| match (pick, decisive) {
                (0, false) => s.arrival_total(),
                (0, true) => s.arrival_decisive(),
                (_, false) => s.any_music_total(),
                (_, true) => s.any_music_decisive(),
            };
            let _ = write!(o, "{:<58} {:>6}", label, scores.first().map_or(0, |s| tot(s).1));
            for s in &scores {
                let _ = write!(o, " {:>6}", tot(s).0);
            }
            let _ = writeln!(o);
        }
        let _ = writeln!(o, "  * sinra and sinraslo are adjacent lines: this row cannot tell k=+1 from k=+2 (see the ranking below)\n");
    }

    let _ = writeln!(o, "== Evidence C: battle music and the ids with no line ==");
    let _ = write!(o, "{:<58} {:>6}", "BMUSC uses (distinct field + operand) naming `bat`", scores.first().map_or(0, |s| s.battle_uses));
    for s in &scores {
        let _ = write!(o, " {:>6}", s.battle_on_bat);
    }
    let _ = writeln!(o);
    let _ = write!(o, "{:<58} {:>6}", "distinct arrival ids with no line", scores.first().map_or(0, |s| s.arrival_ids));
    for s in &scores {
        let _ = write!(o, " {:>6}", s.arrival_ids_without_track);
    }
    let _ = writeln!(o);
    for id in [1u16, 2, 3, 6, 7, 8, 11, 47, 96] {
        let _ = write!(o, "{:<65}", format!("what id {id} names"));
        for k in KS {
            let _ = write!(o, " {:>6}", track_under_k(list, id, k).unwrap_or("-"));
        }
        let _ = writeln!(o);
    }

    let _ = writeln!(o, "\n== Evidence D: every offset d in -96..=96 ranked by member fields (regions with one allowed track) with an id = line(track) + d; chosen rule d = +1 ==");
    for (label, scope) in [("arrival id", Scope::Arrival), ("any MUSIC id", Scope::AnyMusic), ("any frame id", Scope::AnyFrame)] {
        let (votes, members) = offset_ranking(records, list, scope);
        let top: Vec<String> = ranked(&votes).into_iter().take(5).map(|(d, n)| format!("d={d:+}: {n}")).collect();
        let _ = writeln!(o, "{label:<14} ({members} member fields) top 5: {}", top.join(", "));
    }
    o
}

// --- Evidence E: the size of a piece of music data against the length of its audio ---------------

/// Duration in seconds of an Ogg Vorbis file: the granule position of its last page (the number of
/// samples) over the sample rate of the identification header.
fn ogg_seconds(b: &[u8]) -> Option<f64> {
    let head = b.get(..b.len().min(512))?;
    let at = head.windows(7).position(|w| w == b"\x01vorbis")?;
    // "\x01vorbis", version u32, channels u8, sample rate u32
    let rate = u32::from_le_bytes(b.get(at + 12..at + 16)?.try_into().ok()?);
    // the last page has the end-of-stream flag
    let last = b.windows(6).rposition(|w| &w[..4] == b"OggS" && w[4] == 0 && w[5] & 0x04 != 0)?;
    let granule = u64::from_le_bytes(b.get(last + 6..last + 14)?.try_into().ok()?);
    (rate > 0).then(|| granule as f64 / f64::from(rate))
}

/// Duration in seconds of a RIFF/WAVE file: the data chunk over the byte rate of the fmt chunk.
fn wav_seconds(b: &[u8]) -> Option<f64> {
    if b.get(..4)? != b"RIFF" || b.get(8..12)? != b"WAVE" {
        return None;
    }
    let (mut pos, mut byte_rate) = (12usize, None);
    while pos + 8 <= b.len() {
        let id = b.get(pos..pos + 4)?;
        let size = u32::from_le_bytes(b.get(pos + 4..pos + 8)?.try_into().ok()?) as usize;
        let body = pos + 8;
        if id == b"fmt " {
            byte_rate = Some(u32::from_le_bytes(b.get(body + 8..body + 12)?.try_into().ok()?));
        } else if id == b"data" {
            let bytes = size.min(b.len() - body);
            return byte_rate.filter(|&r| r > 0).map(|r| bytes as f64 / f64::from(r));
        }
        pos = body.checked_add(size)?.checked_add(size & 1)?;
    }
    None
}

/// Length of the track in seconds (`music_ogg/<name>.ogg`, else `music/<name>.wav`).
pub fn track_seconds(audio: &Path, name: &str) -> Option<f64> {
    if let Ok(bytes) = std::fs::read(audio.join("music_ogg").join(format!("{name}.ogg"))) {
        return ogg_seconds(&bytes);
    }
    wav_seconds(&std::fs::read(audio.join("music").join(format!("{name}.wav"))).ok()?)
}

/// The size in bytes of the AKAO frame of each id (the median over the fields that carry it).
/// Frames shorter than 40 bytes hold no sequence data (the 28-byte frames of id 1) and are left
/// out, for every k alike.
pub fn frame_sizes(records: &[FieldRecord]) -> BTreeMap<u16, usize> {
    let mut by: BTreeMap<u16, Vec<usize>> = BTreeMap::new();
    for r in records {
        for (id, len) in r.blocks.iter().zip(&r.block_lens) {
            if let Some(id) = id.filter(|_| *len >= 40) {
                by.entry(id).or_default().push(*len);
            }
        }
    }
    by.into_iter()
        .filter_map(|(id, mut v)| {
            v.sort_unstable();
            v.get(v.len() / 2).map(|&m| (id, m))
        })
        .collect()
}

fn pearson(xs: &[f64], ys: &[f64]) -> f64 {
    let n = xs.len() as f64;
    let (mx, my) = (xs.iter().sum::<f64>() / n, ys.iter().sum::<f64>() / n);
    let cov: f64 = xs.iter().zip(ys).map(|(x, y)| (x - mx) * (y - my)).sum();
    let (vx, vy): (f64, f64) = (xs.iter().map(|x| (x - mx).powi(2)).sum(), ys.iter().map(|y| (y - my).powi(2)).sum());
    cov / (vx * vy).sqrt()
}

/// For every offset `k` in `-6..=8`: `(k, pairs, pearson r)` between the frame size of each id and
/// the length in seconds of the track line `id - k` names. No field name enters this test.
pub fn size_vs_duration(records: &[FieldRecord], list: &MusicList, audio: &Path) -> Vec<(i32, usize, f64)> {
    let sizes = frame_sizes(records);
    let seconds: BTreeMap<&str, f64> = list.names().iter().filter_map(|n| track_seconds(audio, n).map(|s| (n.as_str(), s))).collect();
    (-6..=8)
        .map(|k| {
            let (mut xs, mut ys) = (Vec::new(), Vec::new());
            for (&id, &size) in &sizes {
                if let Some(s) = track_under_k(list, id, k).and_then(|t| seconds.get(t)) {
                    xs.push(size as f64);
                    ys.push(*s);
                }
            }
            (k, xs.len(), pearson(&xs, &ys))
        })
        .collect()
}

/// Which fields use a track under the chosen rule, for the reverse reading of the table.
#[derive(Debug, Clone)]
pub struct TrackUse {
    pub id: u16,
    pub track: Option<String>,
    pub fields: Vec<String>,
}

/// For every frame id some `MUSIC` uses: the fields that use it (any slot), in archive order.
pub fn track_usage(records: &[FieldRecord], list: &MusicList) -> Vec<TrackUse> {
    let mut by: BTreeMap<u16, Vec<String>> = BTreeMap::new();
    for r in records {
        for id in r.music.distinct_music() {
            by.entry(id).or_default().push(r.name.clone());
        }
    }
    by.into_iter().map(|(id, fields)| TrackUse { id, track: list.track_for_frame_id(id).map(str::to_string), fields }).collect()
}

/// The alphabetic stem of a field name (`nmkin_2` -> `nmkin`): its family.
pub fn family(name: &str) -> &str {
    let end = name.find(|c: char| !c.is_ascii_lowercase()).unwrap_or(name.len());
    name.get(..end).unwrap_or(name)
}

/// Evidence E as text: the correlation of frame size and track length for every offset.
pub fn size_report(records: &[FieldRecord], list: &MusicList, audio: &Path) -> String {
    let mut o = String::new();
    let _ = writeln!(o, "== Evidence E: size of each id's AKAO frame (bytes) against the length of the track line `id - k` names (seconds); no field name used ==");
    let _ = writeln!(o, "{:>4} {:>6} {:>10}", "k", "pairs", "pearson r");
    for (k, pairs, r) in size_vs_duration(records, list, audio) {
        let _ = writeln!(o, "{k:>+4} {pairs:>6} {r:>10.3}{}", if k == 1 { "   <- chosen rule" } else { "" });
    }
    let sizes = frame_sizes(records);
    let mut short: Vec<(usize, u16)> = sizes.iter().map(|(&id, &s)| (s, id)).collect();
    short.sort_unstable();
    let text: Vec<String> = short
        .into_iter()
        .take(6)
        .map(|(s, id)| format!("id {id} {} {s} B {}", list.track_for_frame_id(id).unwrap_or("-"), list.track_for_frame_id(id).and_then(|t| track_seconds(audio, t)).map_or("?".to_string(), |s| format!("{s:.1} s"))))
        .collect();
    let _ = writeln!(o, "the six smallest frames and the track each names (k=+1): {}", text.join("; "));
    o
}

pub fn tracks_report(records: &[FieldRecord], list: &MusicList) -> String {
    let mut o = String::new();
    let _ = writeln!(o, "== reverse reading: the track each frame id names (chosen rule) and the field families that use it ==");
    for t in track_usage(records, list) {
        let mut fam: BTreeMap<&str, usize> = BTreeMap::new();
        for f in &t.fields {
            *fam.entry(family(f)).or_default() += 1;
        }
        let mut fam: Vec<(&str, usize)> = fam.into_iter().collect();
        fam.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        let fam: Vec<String> = fam.into_iter().take(6).map(|(f, n)| format!("{f}x{n}")).collect();
        let _ = writeln!(o, "id {:>2} {:<9} {:>3} fields  {}", t.id, t.track.as_deref().unwrap_or("-"), t.fields.len(), fam.join(", "));
    }
    o
}

// ---------------------------------------------------------------------------------------
// Ambiguity of the extraction policy
// ---------------------------------------------------------------------------------------

/// How many distinct ids the fields have for one kind of use.
#[derive(Debug, Clone, Default)]
pub struct Spread {
    /// Fields with at least one such use.
    pub fields: usize,
    /// ... with exactly one distinct id / with several.
    pub one: usize,
    pub several: usize,
    /// Up to ten fields with several, as `name[ids]`.
    pub examples: Vec<String>,
}

pub fn spread(records: &[FieldRecord], ids: impl Fn(&FieldRecord) -> Vec<u16>) -> Spread {
    let mut s = Spread::default();
    for r in records {
        let v = ids(r);
        match v.len() {
            0 => {}
            1 => {
                s.fields += 1;
                s.one += 1;
            }
            _ => {
                s.fields += 1;
                s.several += 1;
                if s.examples.len() < 10 {
                    s.examples.push(format!("{}{:?}", r.name, v));
                }
            }
        }
    }
    s
}

#[derive(Debug, Clone)]
pub struct Ambiguity {
    pub fields: usize,
    pub fields_with_blocks: usize,
    /// Fields with real AKAO frames but no `MUSIC` use at all.
    pub frames_without_music: usize,
    pub music_any: Spread,
    pub arrival: Spread,
    pub battle: Spread,
    pub after_battle: Spread,
    /// Fields whose only `MUSIC` uses are in slots >= 1 (no arrival).
    pub music_but_no_arrival: usize,
    /// Fields without any AKAO block (the previous field's music goes on).
    pub no_blocks: usize,
    /// Fields with both an `FMUSC` and an arrival, and how many of them have the same first id.
    pub after_battle_with_arrival: usize,
    pub after_battle_equals_arrival: usize,
    /// Fields whose arrival id has no track under the chosen rule.
    pub arrival_without_track: Vec<String>,
    /// Among the fields with several arrival ids: the first use is conditional / no slot-0 use
    /// at all is unconditional.
    pub several_first_conditional: usize,
    pub several_all_conditional: usize,
    /// Fields with a slot-0 `MUSIC`: exactly one unconditional init-part use / none.
    pub slot0_fields: usize,
    pub slot0_one_unconditional_init: usize,
    pub slot0_no_unconditional_init: usize,
    /// Of the fields with exactly one unconditional init-part use: those where the first slot-0
    /// use is another one (a branch-guarded use comes first), and those with a slot-0 use of
    /// another id anywhere.
    pub one_unconditional_but_first_is_another: Vec<String>,
    pub one_unconditional_with_other_ids: usize,
    /// `BMUSC` instructions extracted in slot 0 / in slots >= 1.
    pub battle_uses_slot0: usize,
    pub battle_uses_other: usize,
    /// Fields with a slot-0 `BMUSC`; of them those whose first `BMUSC` (script order) is in a slot
    /// >= 1; and the fields whose `BMUSC` are all in slots >= 1.
    pub battle_fields_with_slot0: usize,
    pub battle_first_in_other_slot_despite_slot0: Vec<String>,
    pub battle_only_in_other_slots: Vec<String>,
}

pub fn ambiguity(records: &[FieldRecord], list: &MusicList) -> Ambiguity {
    let arrival = spread(records, FieldRecord::slot0_music);
    let mut a = Ambiguity {
        fields: records.len(),
        fields_with_blocks: records.iter().filter(|r| !r.blocks.is_empty()).count(),
        frames_without_music: records.iter().filter(|r| r.frames() > 0 && r.music.music.is_empty()).count(),
        music_any: spread(records, |r| r.music.distinct_music()),
        battle: spread(records, |r| r.music.distinct_battle()),
        after_battle: spread(records, |r| r.music.distinct_after_battle()),
        music_but_no_arrival: records.iter().filter(|r| !r.music.music.is_empty() && r.music.arrival().is_none()).count(),
        no_blocks: records.iter().filter(|r| r.blocks.is_empty()).count(),
        after_battle_with_arrival: 0,
        after_battle_equals_arrival: 0,
        arrival_without_track: Vec::new(),
        several_first_conditional: 0,
        several_all_conditional: 0,
        slot0_fields: 0,
        slot0_one_unconditional_init: 0,
        slot0_no_unconditional_init: 0,
        one_unconditional_but_first_is_another: Vec::new(),
        one_unconditional_with_other_ids: 0,
        battle_uses_slot0: 0,
        battle_uses_other: 0,
        battle_fields_with_slot0: 0,
        battle_first_in_other_slot_despite_slot0: Vec::new(),
        battle_only_in_other_slots: Vec::new(),
        arrival,
    };
    for r in records {
        if let (Some(after), Some(arrival)) = (r.music.after_battle(), r.music.arrival()) {
            a.after_battle_with_arrival += 1;
            a.after_battle_equals_arrival += usize::from(after == arrival);
        }
        if r.music.arrival().is_some_and(|id| list.track_for_frame_id(id).is_none()) {
            a.arrival_without_track.push(r.name.clone());
        }
        let uses = r.slot0_uses();
        if !uses.is_empty() {
            a.slot0_fields += 1;
            let unconditional_init: Vec<&&MusicUse> = uses.iter().filter(|u| !u.conditional && u.phase == Some(Phase::Init)).collect();
            a.slot0_one_unconditional_init += usize::from(unconditional_init.len() == 1);
            a.slot0_no_unconditional_init += usize::from(unconditional_init.is_empty());
            if let [only] = unconditional_init.as_slice() {
                if uses.first().is_some_and(|first| (first.entity, first.offset) != (only.entity, only.offset)) {
                    a.one_unconditional_but_first_is_another.push(r.name.clone());
                }
                a.one_unconditional_with_other_ids += usize::from(uses.iter().any(|u| u.frame_id != only.frame_id));
            }
            if r.slot0_music().len() > 1 {
                a.several_first_conditional += usize::from(uses.first().is_some_and(|u| u.conditional));
                a.several_all_conditional += usize::from(uses.iter().all(|u| u.conditional));
            }
        }
        a.battle_uses_slot0 += r.music.battle.iter().filter(|u| u.slot == INIT_SCRIPT).count();
        a.battle_uses_other += r.music.battle.iter().filter(|u| u.slot != INIT_SCRIPT).count();
        let has_slot0_battle = r.music.battle.iter().any(|u| u.slot == INIT_SCRIPT);
        a.battle_fields_with_slot0 += usize::from(has_slot0_battle);
        if has_slot0_battle && r.music.battle.first().is_some_and(|u| u.slot != INIT_SCRIPT) {
            a.battle_first_in_other_slot_despite_slot0.push(r.name.clone());
        }
        if !r.music.battle.is_empty() && !has_slot0_battle {
            a.battle_only_in_other_slots.push(r.name.clone());
        }
    }
    a
}

pub fn ambiguity_report(records: &[FieldRecord], list: &MusicList) -> String {
    let a = ambiguity(records, list);
    let mut o = String::new();
    let _ = writeln!(o, "== Ambiguity of the extraction policy (chosen rule k=+1) ==");
    let _ = writeln!(o, "fields {}; with AKAO blocks {}; without any block {}; with real frames but no MUSIC use {}", a.fields, a.fields_with_blocks, a.no_blocks, a.frames_without_music);
    for (label, s) in [("MUSIC (any slot)", &a.music_any), ("arrival (slot-0 MUSIC)", &a.arrival), ("BMUSC", &a.battle), ("FMUSC", &a.after_battle)] {
        let _ = writeln!(o, "{label}: {} fields with a use; exactly one distinct id {}; several {}; e.g. {}", s.fields, s.one, s.several, s.examples.join(" "));
    }
    let _ = writeln!(o, "fields with MUSIC but no slot-0 MUSIC (no arrival): {}", a.music_but_no_arrival);
    let _ = writeln!(o, "FMUSC: {} fields have both an FMUSC and an arrival; the first FMUSC id equals the arrival id in {}", a.after_battle_with_arrival, a.after_battle_equals_arrival);
    let _ = writeln!(o, "arrival id with no track (id 1): {} fields: {}", a.arrival_without_track.len(), a.arrival_without_track.join(" "));
    let _ = writeln!(
        o,
        "slot-0 fields {}: exactly one unconditional init-part MUSIC {}; none {}. Of the {} fields with several arrival ids the first use is conditional in {} and every use is conditional in {}",
        a.slot0_fields, a.slot0_one_unconditional_init, a.slot0_no_unconditional_init, a.arrival.several, a.several_first_conditional, a.several_all_conditional
    );
    let _ = writeln!(
        o,
        "of the {} fields with exactly one unconditional init-part MUSIC: the first slot-0 use is another one in {} ({}); a slot-0 MUSIC of another id exists in {}",
        a.slot0_one_unconditional_init,
        a.one_unconditional_but_first_is_another.len(),
        a.one_unconditional_but_first_is_another.join(" "),
        a.one_unconditional_with_other_ids
    );
    let _ = writeln!(o, "BMUSC instructions: {} in slot 0, {} in other slots", a.battle_uses_slot0, a.battle_uses_other);
    let _ = writeln!(
        o,
        "BMUSC: {} of {} fields have one in slot 0 (the first BMUSC is in a later slot than 0 in {} of them: {}); only in other slots: {}",
        a.battle_fields_with_slot0,
        a.battle.fields,
        a.battle_first_in_other_slot_despite_slot0.len(),
        a.battle_first_in_other_slot_despite_slot0.join(" "),
        a.battle_only_in_other_slots.join(" ")
    );
    o
}

/// Every music instruction of the survey that picks a block with frame id `id`, as text.
pub fn uses_of_frame(records: &[FieldRecord], id: u16) -> Vec<String> {
    let mut out = Vec::new();
    for r in records {
        for o in &r.ops {
            let frame = o.operand.and_then(|v| usize::try_from(v).ok()).and_then(|b| r.blocks.get(b).copied().flatten());
            if frame == Some(id) {
                out.push(format!(
                    "{} {} block {} (entity {}, slot {}, {}{})",
                    r.name,
                    o.mnemonic,
                    o.operand.unwrap_or(-1),
                    o.entity,
                    o.slot,
                    o.phase.map_or("not slot 0".to_string(), |p| format!("{p:?} part")),
                    if o.conditional { ", conditional" } else { "" }
                ));
            }
        }
    }
    out
}
