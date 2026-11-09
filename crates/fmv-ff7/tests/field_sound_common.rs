//! Shared survey code for `tests/field_sound_real.rs` and `examples/field_sound_survey.rs`.
//!
//! Reads the owner's `flevel.lgp` and `audio.fmt` (read-only; `audio.dat` is opened only to decode the clips the
//! reviewed fields need) and records ids, offsets, counts and field names only: no game bytes are kept or printed.
#![allow(dead_code)]

use fmv_ff7::field::FieldFile;
use fmv_ff7::field_sound::{exit_events, flip_events, link_events, start_up, Ambient, Call, Part, RunInfo, Source, Stop, Timed};
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::lzss;
use fmv_ff7::script::{FieldScript, Maplist, ScriptRef, INIT_SCRIPT, SCRIPTS_PER_ENTITY};
use fmv_ff7::sound::SfxBank;
use fmv_ff7::story::StoryState;
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

/// Field entries are the ones whose names have no '.', excluding the name list.
pub fn is_field_name(name: &str) -> bool {
    !name.contains('.') && !name.eq_ignore_ascii_case("maplist")
}

/// The sound bank of the audio folder (`sound/audio.fmt` and `sound/audio.dat`).
pub fn open_bank(audio: &Path) -> Result<SfxBank, String> {
    SfxBank::open(&audio.join("sound").join("audio.fmt"), &audio.join("sound").join("audio.dat")).map_err(|e| e.to_string())
}

/// Whether the clip of sound id `id` loops (the clip's loop flag in `audio.fmt`).
pub fn loops(bank: &SfxBank, id: u16) -> bool {
    bank.info_for_sound_id(id).is_some_and(|i| i.loop_region.is_some())
}

/// Every field of the archive, decompressed, with its maplist.
pub struct Archive {
    pub fields: Vec<(String, FieldFile)>,
    pub maplist: Maplist,
}

pub fn load_archive(dir: &Path) -> Result<Archive, String> {
    let mut lgp: LgpArchive<File> = LgpArchive::open_path(flevel_path(dir)).map_err(|e| e.to_string())?;
    let maplist = Maplist::from_archive(&mut lgp).map_err(|e| e.to_string())?;
    let names: Vec<String> = lgp.list().iter().filter(|e| is_field_name(&e.name)).map(|e| e.name.clone()).collect();
    let mut fields = Vec::new();
    for name in names {
        let Ok(stored) = lgp.read(&name) else { continue };
        let Ok(bytes) = lzss::decompress(&stored) else { continue };
        let Ok(field) = FieldFile::parse(bytes) else { continue };
        fields.push((name, field));
    }
    Ok(Archive { fields, maplist })
}

/// Where in a script an opcode lies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Where {
    /// Script 0 up to its first RET.
    Init,
    /// Script 0 after its first RET.
    Main,
    /// Any other slot.
    Other,
}

/// One sound play or stop found in a script (a static census, no run).
#[derive(Debug, Clone)]
pub struct Op {
    pub field: String,
    pub entity: usize,
    pub slot: usize,
    pub offset: usize,
    pub call: Call,
    pub at: Where,
    /// Between a forward jump and its target (an IF's body, or the code a jump skips).
    pub guarded: bool,
    /// Between a backward jump's target and the jump.
    pub in_loop: bool,
}

#[derive(Debug, Default)]
pub struct Census {
    pub fields: usize,
    pub ops: Vec<Op>,
    /// Sound opcodes whose id or pan names a variable.
    pub banked: usize,
    /// `AKAO` plays (operation 40 to 43) with a non-zero `p3`, `p4` or `p5`.
    pub akao_tail_nonzero: usize,
    /// `AKAO` and `AKAO2` plays with `p2` = 0, by `p1`: the pan-like filler of a stop.
    pub stop_pans: BTreeMap<u8, usize>,
    /// How many `AKAO`/`AKAO2` instructions there are of each operation (every one, plays or not).
    pub akao_operations: BTreeMap<i32, usize>,
}

/// Count every `SOUND` and every `AKAO`/`AKAO2` sound play of every distinct script of every field.
pub fn census(fields: &[(String, FieldFile)]) -> Census {
    let mut out = Census::default();
    for (name, field) in fields {
        let Ok(script) = FieldScript::from_field(field) else { continue };
        out.fields += 1;
        let mut seen = BTreeSet::new();
        for entity in 0..script.entity_count() {
            for slot in 0..SCRIPTS_PER_ENTITY {
                let Some(span) = script.span(entity, slot) else { continue };
                if !seen.insert(span.start) {
                    continue;
                }
                let Ok(d) = script.disassemble_partial(entity, slot) else { continue };
                let first_ret = (slot == INIT_SCRIPT).then(|| d.instructions.iter().find(|i| i.opcode == 0).map(|i| i.offset)).flatten();
                let forward: Vec<(usize, usize)> = d.instructions.iter().filter(|i| i.is_jump()).filter_map(|i| i.jump_target.filter(|&t| t > i.offset).map(|t| (i.end(), t))).collect();
                let backward: Vec<(usize, usize)> = d.instructions.iter().filter(|i| i.is_jump()).filter_map(|i| i.jump_target.filter(|&t| t <= i.offset).map(|t| (t, i.end()))).collect();
                for ins in &d.instructions {
                    if (ins.opcode == 0xF2 || ins.opcode == 0xDA) && ins.literal("op").is_some() {
                        *out.akao_operations.entry(ins.literal("op").unwrap_or(-1)).or_default() += 1;
                    }
                    let call = match Call::of(ins) {
                        Ok(Some(call)) => call,
                        Ok(None) => continue,
                        Err(_) => {
                            out.banked += 1;
                            continue;
                        }
                    };
                    if call.source != Source::Sound {
                        if ["p3", "p4", "p5"].iter().any(|n| ins.literal(n).unwrap_or(0) != 0) {
                            out.akao_tail_nonzero += 1;
                        }
                        if call.id == 0 {
                            *out.stop_pans.entry(call.pan).or_default() += 1;
                        }
                    }
                    let at = match (slot == INIT_SCRIPT, first_ret) {
                        (true, Some(ret)) if ins.offset > ret => Where::Main,
                        (true, _) => Where::Init,
                        _ => Where::Other,
                    };
                    out.ops.push(Op {
                        field: name.clone(),
                        entity,
                        slot,
                        offset: ins.offset,
                        call,
                        at,
                        guarded: forward.iter().any(|&(f, t)| f <= ins.offset && ins.offset < t),
                        in_loop: backward.iter().any(|&(t, e)| t <= ins.offset && ins.offset < e),
                    });
                }
            }
        }
    }
    out
}

/// What the start-ups of all fields come to for one story.
#[derive(Debug, Default)]
pub struct AmbientSurvey {
    pub moment: u16,
    pub per_field: BTreeMap<String, Vec<Ambient>>,
    pub unjudged: usize,
    pub unjudged_fields: usize,
    pub left_open: usize,
    pub banked: u32,
    pub stops: BTreeMap<&'static str, usize>,
    /// Which untimed opcodes stopped runs, by opcode.
    pub untimed: BTreeMap<u8, usize>,
    /// The tests that stopped runs, as the script states them, and how many.
    pub tests: BTreeMap<String, usize>,
}

pub fn ambient_survey(fields: &[(String, FieldFile)], bank: &SfxBank, moment: u16) -> AmbientSurvey {
    let story = StoryState::at(moment);
    let mut out = AmbientSurvey { moment, ..AmbientSurvey::default() };
    for (name, field) in fields {
        let Ok(script) = FieldScript::from_field(field) else { continue };
        let start = start_up(&script, &story);
        let ambient = start.ambient(|id| loops(bank, id));
        out.left_open += start.ambient_left_open(|id| loops(bank, id));
        out.unjudged += start.unjudged.len();
        out.unjudged_fields += usize::from(!start.unjudged.is_empty());
        out.banked += start.banked;
        for (label, n) in start.stops() {
            *out.stops.entry(label).or_default() += n;
        }
        for run in &start.runs {
            if let Stop::Unknown { opcode, .. } = run.stop {
                *out.untimed.entry(opcode).or_default() += 1;
            }
        }
        for u in &start.unjudged {
            *out.tests.entry(u.test.clone()).or_default() += 1;
        }
        if !ambient.is_empty() {
            out.per_field.insert(name.clone(), ambient);
        }
    }
    out
}

/// `(channel, id, at in frames)` of ambient loops, for a short line.
pub fn show_ambient(list: &[Ambient]) -> String {
    list.iter().map(|a| if a.at == 0 { format!("ch{}:{}", a.channel, a.id) } else { format!("ch{}:{}@{}f", a.channel, a.id, a.at) }).collect::<Vec<_>>().join(" ")
}

/// `(at frames, channel, id)` of events, for a short line.
pub fn show_events(events: &[Timed]) -> String {
    events.iter().map(|e| format!("{}f ch{}:{}", e.at, e.call.channel, e.call.id)).collect::<Vec<_>>().join(", ")
}

pub fn show_stops(runs: &[RunInfo]) -> String {
    runs.iter().map(|r| r.stop.label()).collect::<Vec<_>>().join(" > ")
}

/// What one reviewed field plays, by every plan, for the stories given.
#[derive(Debug, Default)]
pub struct Reviewed {
    pub field: String,
    /// `(moment, ambient)`.
    pub ambient: Vec<(u16, Vec<Ambient>)>,
    /// `(needs OK, events)`.
    pub links: Vec<(bool, Vec<Timed>)>,
    /// `(on, events)`.
    pub flips: Vec<(bool, Vec<Timed>)>,
    pub exits: Vec<(String, Vec<Timed>)>,
    /// Every sound id any of them plays (ids of stops left out).
    pub ids: BTreeSet<u16>,
}

pub fn reviewed_field(archive: &Archive, bank: &SfxBank, name: &str, moments: &[u16]) -> Option<Reviewed> {
    let (_, field) = archive.fields.iter().find(|(n, _)| n.eq_ignore_ascii_case(name))?;
    let script = FieldScript::from_field(field).ok()?;
    let mesh = field.walkmesh().ok()?;
    let mut out = Reviewed { field: name.to_string(), ..Reviewed::default() };
    for &moment in moments {
        let story = StoryState::at(moment);
        let start = start_up(&script, &story);
        let ambient = start.ambient(|id| loops(bank, id));
        out.ids.extend(ambient.iter().map(|a| a.id));
        out.ambient.push((moment, ambient));
        // links, flips and exits are judged for the story of the moment too (the last moment's are kept)
        out.links.clear();
        out.flips.clear();
        out.exits.clear();
        for link in link_events(&script, &mesh, &story) {
            out.ids.extend(link.events.iter().map(|e| e.call.id).filter(|id| *id != 0));
            out.links.push((link.needs_ok, link.events));
        }
        let mut lifted = story.clone();
        lifted.set_bit(1, 225, 0, true);
        for flip in flip_events(&script, &lifted) {
            out.ids.extend(flip.events.iter().map(|e| e.call.id).filter(|id| *id != 0));
            out.flips.push((flip.on, flip.events));
        }
        for exit in exit_events(&script, &archive.maplist, &story) {
            out.ids.extend(exit.events.iter().map(|e| e.call.id).filter(|id| *id != 0));
            out.exits.push((exit.target, exit.events));
        }
    }
    Some(out)
}

/// Every plan over every field, for the story at `moment`: the ids any of them plays, and how many fields have each plan.
#[derive(Debug, Default)]
pub struct Plans {
    pub ids: BTreeSet<u16>,
    pub fields_with_links: BTreeSet<String>,
    pub links: usize,
    pub fields_with_flips: BTreeSet<String>,
    pub flips: usize,
    pub fields_with_exits: BTreeSet<String>,
    pub exits: usize,
    pub link_stops: BTreeMap<&'static str, usize>,
}

pub fn all_plans(archive: &Archive, bank: &SfxBank, moment: u16) -> Plans {
    let story = StoryState::at(moment);
    let mut lifted = story.clone();
    lifted.set_bit(1, 225, 0, true);
    let mut out = Plans::default();
    for (name, field) in &archive.fields {
        let Ok(script) = FieldScript::from_field(field) else { continue };
        let start = start_up(&script, &story);
        out.ids.extend(start.ambient(|id| loops(bank, id)).iter().map(|a| a.id));
        if let Ok(mesh) = field.walkmesh() {
            for link in link_events(&script, &mesh, &story) {
                out.ids.extend(link.events.iter().map(|e| e.call.id).filter(|id| *id != 0));
                out.fields_with_links.insert(name.clone());
                out.links += 1;
                for run in link.runs.iter().skip(1) {
                    *out.link_stops.entry(run.stop.label()).or_default() += 1;
                }
            }
        }
        for flip in flip_events(&script, &lifted) {
            out.ids.extend(flip.events.iter().map(|e| e.call.id).filter(|id| *id != 0));
            out.fields_with_flips.insert(name.clone());
            out.flips += 1;
        }
        for exit in exit_events(&script, &archive.maplist, &story) {
            out.ids.extend(exit.events.iter().map(|e| e.call.id).filter(|id| *id != 0));
            out.fields_with_exits.insert(name.clone());
            out.exits += 1;
        }
    }
    out
}

/// The text the survey and the test print about the census.
pub fn census_report(census: &Census, bank: Option<&SfxBank>) -> String {
    let mut out = String::new();
    let sounds: Vec<&Op> = census.ops.iter().filter(|o| o.call.source == Source::Sound).collect();
    let akao: Vec<&Op> = census.ops.iter().filter(|o| o.call.source != Source::Sound).collect();
    let fields = |ops: &[&Op]| ops.iter().map(|o| o.field.as_str()).collect::<BTreeSet<_>>().len();
    let either: BTreeSet<&str> = census.ops.iter().map(|o| o.field.as_str()).collect();
    let _ = writeln!(out, "{} fields; SOUND {} in {} fields; AKAO/AKAO2 plays {} in {} fields; either {} fields; banked {}", census.fields, sounds.len(), fields(&sounds), akao.len(), fields(&akao), either.len(), census.banked);
    let _ = writeln!(out, "AKAO plays with p3/p4/p5 not 0: {}; pans of the stops (id 0): {:?}", census.akao_tail_nonzero, census.stop_pans);
    let _ = writeln!(out, "AKAO operations (every one): {:?}", census.akao_operations);
    for at in [Where::Init, Where::Main, Where::Other] {
        let of = |ops: &[&Op]| ops.iter().filter(|o| o.at == at).count();
        let _ = writeln!(out, "  {at:?}: SOUND {} AKAO {}", of(&sounds), of(&akao));
    }
    if let Some(bank) = bank {
        let init: Vec<&Op> = census.ops.iter().filter(|o| o.at == Where::Init).collect();
        let stops = init.iter().filter(|o| o.call.id == 0).count();
        let loop_plays = init.iter().filter(|o| o.call.id != 0 && loops(bank, o.call.id)).count();
        let _ = writeln!(out, "init-part plays: {} = {stops} stops + {loop_plays} loop clips + {} other", init.len(), init.len() - stops - loop_plays);
    }
    out
}

/// The ids of the plays that name a clip: those past 0.
pub fn ids_of(ops: &[Op]) -> BTreeSet<u16> {
    ops.iter().map(|o| o.call.id).filter(|id| *id != 0).collect()
}

/// A `ScriptRef` as `entity:slot` for lines.
pub fn show_ref(r: ScriptRef) -> String {
    format!("{}:{}", r.entity, r.slot)
}

/// Run only the init and main parts of one entity, for checks that want a single run.
pub fn parts() -> [Part; 2] {
    [Part::Init, Part::Main]
}
