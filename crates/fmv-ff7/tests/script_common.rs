//! Shared survey code for `tests/script_real_data.rs` and `examples/script_dump.rs`.
//!
//! Reads the owner's `flevel.lgp` (read-only) and walks every field's script section. It
//! records counts, offsets and opcode statistics only; it never stores or prints game bytes
//! (the dialog survey counts strings, it does not keep text).
#![allow(dead_code)]

use fmv_ff7::field::{FieldFile, Walkmesh};
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::lzss;
use fmv_ff7::script::{
    check_placement, mnemonic, parse_model_list, resolve_gateways, triangle_contains_xy, Arg, FieldScript, Maplist, Phase, ScriptEnd,
    WalkmeshFit, INIT_SCRIPT, SCRIPTS_PER_ENTITY,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

pub fn data_dir() -> Option<PathBuf> {
    std::env::var_os("FMV_FF7_DATA").map(PathBuf::from)
}

pub fn flevel_path(dir: &Path) -> PathBuf {
    dir.join("field").join("flevel.lgp")
}

/// Field entries are the ones whose names have no '.', excluding the name list.
pub fn is_field_name(name: &str) -> bool {
    !name.contains('.') && !name.eq_ignore_ascii_case("maplist")
}

pub fn load_field(lgp: &mut LgpArchive<std::fs::File>, name: &str) -> Result<FieldFile, String> {
    let stored = lgp.read(name).map_err(|e| e.to_string())?;
    let bytes = lzss::decompress(&stored).map_err(|e| e.to_string())?;
    FieldFile::parse(bytes).map_err(|e| e.to_string())
}

/// One finding with its location (no game bytes).
#[derive(Debug, Clone)]
pub struct Problem {
    pub field: String,
    pub entity: usize,
    pub script: usize,
    pub what: String,
}

/// Totals over all fields.
#[derive(Debug, Default)]
pub struct Survey {
    pub fields: usize,
    pub fields_with_script_parse_error: Vec<String>,
    pub header_versions: BTreeMap<(u8, u8), usize>,
    pub string_offset_is_code_end: usize,
    pub scale_matches_loader: usize,
    pub entities: usize,
    // entry tables
    pub slots: usize,
    pub slots_without_code: Vec<Problem>,
    pub slots_sharing_an_entry: usize,
    // scripts
    pub distinct_scripts: usize,
    pub tile_exactly: usize,
    pub instructions: usize,
    pub desyncs: Vec<Problem>,
    pub ends: BTreeMap<String, usize>,
    pub falls_through_into_ret: usize,
    pub falls_through_into_other: Vec<Problem>,
    // jumps
    pub jumps: usize,
    pub jumps_to_instruction: usize,
    /// Jumps whose target is the first byte after the script (the next script's first
    /// instruction, usually a shared `RET`).
    pub jumps_to_script_end: usize,
    pub jumps_to_script_end_ret: usize,
    pub bad_jumps: Vec<Problem>,
    // opcodes
    pub opcode_counts: BTreeMap<u8, usize>,
    pub sub_counts: BTreeMap<(u8, u8), usize>,
    pub stray_bank_instructions: BTreeMap<u8, usize>,
    pub banked_operand_instructions: BTreeMap<u8, usize>,
    // script 0 structure
    pub script0_with_main_part: usize,
    pub script0_without_main_part: usize,
    /// Script 0s whose first `RET` lies in a region a jump skips (so the init/main split by
    /// first `RET` is only a heuristic for them).
    pub script0_first_ret_conditional: usize,
    pub interior_ret_in_other_slots: usize,
    pub header_blank_nonzero: usize,
    // dialogs and AKAO
    pub dialog_tables_ok: usize,
    pub dialog_table_errors: Vec<String>,
    pub dialog_first_pointer_matches: usize,
    pub dialogs: usize,
    pub dialogs_unterminated: Vec<String>,
    /// Decoded dialog characters by class (counts only).
    pub text_ascii: usize,
    pub text_newlines: usize,
    pub text_named_markers: usize,
    pub text_hex_markers: usize,
    pub text_other_glyphs: usize,
    pub akao_blocks: usize,
    pub non_akao_blocks: Vec<(String, u32)>,
    // models and characters
    pub model_loader_exact: usize,
    pub model_count_matches_loader: usize,
    pub char_entities: usize,
    pub fields_where_char_entities_equal_model_count: usize,
    pub char_id_equals_model_index: usize,
    pub char_id_differs_from_model_index: usize,
    pub chars_with_conditional_char: usize,
    // player binding
    pub fields_by_pc_count: BTreeMap<usize, usize>,
    pub pc_character_ids: BTreeMap<u8, usize>,
    pub pc_duplicate_character_in_field: usize,
    // placements
    pub placements: usize,
    pub placements_conditional: usize,
    pub placements_main_phase: usize,
    pub placements_variable_operands: usize,
    pub placement_inside: usize,
    pub placement_outside: Vec<Problem>,
    pub placement_no_triangle: Vec<Problem>,
    pub max_abs_height_error: f64,
    pub height_errors_over_8: usize,
    pub entities_with_literal_placement: usize,
    // exits
    pub gateways_used: usize,
    pub gateways_resolved: usize,
    pub gateways_unresolved: Vec<(String, u16)>,
    pub gateways_target_in_archive: usize,
    pub map_jump_instructions: usize,
    pub map_jumps_resolved: usize,
    pub map_jumps_target_in_archive: usize,
    pub map_jump_destinations_on_triangle: usize,
    /// `(0, 0, 0)` destinations (the position is set by the target field's script).
    pub map_jump_destinations_zero: usize,
    pub map_jump_destinations_off_triangle: Vec<String>,
}

type JumpCheck = (String, String, i16, i16, i16);

/// Walk every field of `lgp`.
pub fn survey_all(lgp: &mut LgpArchive<std::fs::File>) -> Survey {
    let maplist = Maplist::from_archive(lgp).ok();
    let entries: Vec<(usize, String)> =
        lgp.list().iter().enumerate().filter(|(_, e)| is_field_name(&e.name)).map(|(i, e)| (i, e.name.clone())).collect();
    let in_archive: BTreeSet<String> = entries.iter().map(|(_, n)| n.to_ascii_lowercase()).collect();
    let mut s = Survey::default();
    let mut meshes: HashMap<String, Option<Walkmesh>> = HashMap::new();
    let mut jumps_to_check: Vec<JumpCheck> = Vec::new();
    for (index, name) in &entries {
        let Ok(stored) = lgp.read_index(*index) else { continue };
        let Ok(bytes) = lzss::decompress(&stored) else { continue };
        let Ok(field) = FieldFile::parse(bytes) else { continue };
        s.fields += 1;
        survey_field(&mut s, name, &field, maplist.as_ref(), &in_archive, &mut jumps_to_check);
    }
    // MAPJUMP destinations: (x, y) must lie inside the named triangle of the target field
    let mut by_target: BTreeMap<String, Vec<(String, i16, i16, i16)>> = BTreeMap::new();
    for (from, target, x, y, tri) in jumps_to_check {
        by_target.entry(target).or_default().push((from, x, y, tri));
    }
    for (target, list) in by_target {
        let mesh = meshes.entry(target.clone()).or_insert_with(|| {
            let real = lgp.find(&target).map(|e| e.name.clone())?;
            load_field(lgp, &real).ok()?.walkmesh().ok()
        });
        for (from, x, y, tri) in list {
            if (x, y, tri) == (0, 0, 0) {
                s.map_jump_destinations_zero += 1;
                continue;
            }
            let ok = mesh.as_ref().is_some_and(|m| {
                usize::try_from(tri).ok().and_then(|t| m.triangles.get(t)).is_some_and(|t| triangle_contains_xy(t, i32::from(x), i32::from(y)))
            });
            if ok {
                s.map_jump_destinations_on_triangle += 1;
            } else {
                s.map_jump_destinations_off_triangle.push(format!("{from} -> {target} ({x}, {y}) triangle {tri}"));
            }
        }
    }
    s
}

fn survey_field(
    s: &mut Survey,
    name: &str,
    field: &FieldFile,
    maplist: Option<&Maplist>,
    in_archive: &BTreeSet<String>,
    jumps_to_check: &mut Vec<JumpCheck>,
) {
    let script = match FieldScript::from_field(field) {
        Ok(sc) => sc,
        Err(e) => {
            s.fields_with_script_parse_error.push(format!("{name}: {e}"));
            return;
        }
    };
    let layout = script.layout();
    let data = script.bytes();
    *s.header_versions.entry((layout.header.data_version, layout.header.event_version)).or_default() += 1;
    s.entities += script.entity_count();
    if usize::from(layout.header.string_offset) == layout.code_end {
        s.string_offset_is_code_end += 1;
    }
    if field.field_scale().ok() == Some(layout.header.scale) {
        s.scale_matches_loader += 1;
    }
    if layout.header.blank != [0; 3] {
        s.header_blank_nonzero += 1;
    }

    // model loader
    let models = field.section(2).ok().and_then(|sec| parse_model_list(sec).ok());
    if let Some(m) = &models {
        if usize::from(m.declared) == m.models.len() && field.section(2).map(|sec| sec.len()).ok() == Some(m.covered) {
            s.model_loader_exact += 1;
        }
        if usize::from(m.declared) == usize::from(layout.header.model_count) {
            s.model_count_matches_loader += 1;
        }
    }

    // dialogs
    match script.dialog_error() {
        None => {
            s.dialog_tables_ok += 1;
            s.dialogs += script.dialogs().len();
            let first_ok = script.dialogs().first().is_none_or(|d| d.offset == layout.dialog_table + 2 + 2 * script.dialogs().len());
            if first_ok {
                s.dialog_first_pointer_matches += 1;
            }
            for d in script.dialogs() {
                if !d.terminated {
                    s.dialogs_unterminated.push(format!("{name} #{}", d.index));
                }
                // classify the decoded characters (counts only; the text itself is dropped)
                if let Some(t) = script.dialog_text(d.index) {
                    let mut rest = t.text.as_str();
                    while let Some(c) = rest.chars().next() {
                        rest = rest.get(c.len_utf8()..).unwrap_or("");
                        if c == '{' {
                            let end = rest.find('}').unwrap_or(0);
                            let inner = rest.get(..end).unwrap_or("");
                            rest = rest.get(end + 1..).unwrap_or("");
                            if inner.len() == 2 && inner.bytes().all(|b| b.is_ascii_hexdigit()) {
                                s.text_hex_markers += 1;
                            } else {
                                s.text_named_markers += 1;
                            }
                        } else if c == '\n' {
                            s.text_newlines += 1;
                        } else if (' '..='~').contains(&c) {
                            s.text_ascii += 1;
                        } else {
                            s.text_other_glyphs += 1;
                        }
                    }
                }
            }
        }
        Some(e) => s.dialog_table_errors.push(format!("{name}: {e}")),
    }
    for b in script.akao_blocks() {
        s.akao_blocks += 1;
        if !b.is_akao() {
            s.non_akao_blocks.push((name.to_string(), b.offset));
        }
    }

    // scripts
    let mut seen_starts = BTreeSet::new();
    for entity in 0..script.entity_count() {
        for slot in 0..SCRIPTS_PER_ENTITY {
            s.slots += 1;
            let Some(span) = script.span(entity, slot) else {
                let entry = script.layout().entities.get(entity).and_then(|e| e.entries.get(slot)).copied().unwrap_or(0);
                s.slots_without_code.push(Problem {
                    field: name.to_string(),
                    entity,
                    script: slot,
                    what: format!("entry {entry:#06x} outside {:#06x}..{:#06x}", layout.code_start, layout.code_end),
                });
                continue;
            };
            if !seen_starts.insert(span.start) {
                s.slots_sharing_an_entry += 1;
                continue;
            }
            s.distinct_scripts += 1;
            let Ok(d) = script.disassemble_partial(entity, slot) else { continue };
            s.instructions += d.instructions.len();
            let problem = |what: String| Problem { field: name.to_string(), entity, script: slot, what };
            for ins in &d.instructions {
                *s.opcode_counts.entry(ins.opcode).or_default() += 1;
                if let Some((sub, _)) = ins.sub {
                    *s.sub_counts.entry((ins.opcode, sub)).or_default() += 1;
                }
                if ins.stray_banks {
                    *s.stray_bank_instructions.entry(ins.opcode).or_default() += 1;
                }
                if ins.operands.iter().any(|o| o.bank != 0) {
                    *s.banked_operand_instructions.entry(ins.opcode).or_default() += 1;
                }
            }
            if let Some(e) = &d.error {
                s.desyncs.push(problem(e.to_string()));
                continue;
            }
            s.tile_exactly += 1;
            // how the script ends
            let label = match ScriptEnd::of(&d) {
                ScriptEnd::Other(op) => match data.get(span.end) {
                    // no terminator: control falls into whatever follows the span
                    Some(0x00) => {
                        s.falls_through_into_ret += 1;
                        "FallsThroughIntoRet".to_string()
                    }
                    other => {
                        s.falls_through_into_other.push(problem(format!("ends in {} then {other:?}", mnemonic(op).unwrap_or("?"))));
                        "FallsThroughIntoOther".to_string()
                    }
                },
                other => format!("{other:?}"),
            };
            *s.ends.entry(label).or_default() += 1;
            // jumps
            let starts: BTreeSet<usize> = d.instructions.iter().map(|i| i.offset).collect();
            for ins in d.instructions.iter().filter(|i| i.is_jump()) {
                s.jumps += 1;
                match ins.jump_target {
                    Some(t) if starts.contains(&t) => s.jumps_to_instruction += 1,
                    Some(t) if t == span.end => {
                        s.jumps_to_script_end += 1;
                        if data.get(t) == Some(&0x00) {
                            s.jumps_to_script_end_ret += 1;
                        }
                    }
                    other => s.bad_jumps.push(problem(format!("{} at {:#06x} -> {other:?} (span ends {:#06x})", ins.mnemonic, ins.offset, span.end))),
                }
            }
            // RETs that are not the last instruction
            let interior_ret = d.instructions.iter().rev().skip(1).any(|i| i.opcode == 0x00);
            if slot == INIT_SCRIPT {
                if interior_ret {
                    s.script0_with_main_part += 1;
                } else {
                    s.script0_without_main_part += 1;
                }
                // is the first RET (the end of the init part) inside a region some jump skips?
                if let Some(ret) = d.instructions.iter().find(|i| i.opcode == 0x00) {
                    let skipped = d.instructions.iter().any(|j| j.is_jump() && j.jump_target.is_some_and(|t| j.offset < ret.offset && ret.offset < t));
                    if skipped {
                        s.script0_first_ret_conditional += 1;
                    }
                }
            } else if interior_ret {
                s.interior_ret_in_other_slots += 1;
            }
        }
    }

    // script 0 analysis: characters, player binding, placements
    let mesh = field.walkmesh().ok();
    let inits = script.entity_inits(models.as_ref());
    let mut pc_chars = Vec::new();
    let mut char_entities = 0usize;
    for init in &inits {
        if let Some(c) = &init.char_id {
            char_entities += 1;
            if c.conditional {
                s.chars_with_conditional_char += 1;
            }
            match (c.value, init.model_index) {
                (Arg::Literal(id), Some(i)) if usize::from(id) == i => s.char_id_equals_model_index += 1,
                _ => s.char_id_differs_from_model_index += 1,
            }
        }
        if let Some(p) = &init.player {
            if let Arg::Literal(c) = p.value {
                *s.pc_character_ids.entry(c).or_default() += 1;
                pc_chars.push(c);
            }
        }
        let mut literal_placement = false;
        for p in &init.placements {
            s.placements += 1;
            if p.conditional {
                s.placements_conditional += 1;
            }
            if p.phase == Phase::Main {
                s.placements_main_phase += 1;
            }
            if !p.is_literal() {
                s.placements_variable_operands += 1;
            }
            let Some(mesh) = &mesh else { continue };
            let here = |what: String| Problem { field: name.to_string(), entity: init.entity, script: INIT_SCRIPT, what };
            match check_placement(mesh, p) {
                Some(WalkmeshFit::Inside { height_error }) => {
                    s.placement_inside += 1;
                    literal_placement = true;
                    if let Some(e) = height_error {
                        s.max_abs_height_error = s.max_abs_height_error.max(e.abs());
                        if e.abs() > 8.0 {
                            s.height_errors_over_8 += 1;
                        }
                    }
                }
                Some(WalkmeshFit::Outside { distance, containing }) => {
                    literal_placement = true;
                    s.placement_outside.push(here(format!("{:?} at {:#06x} outside its triangle by {distance:.1}, inside {containing:?}", p.kind, p.offset)));
                }
                Some(WalkmeshFit::NoSuchTriangle) => s.placement_no_triangle.push(here(format!("{:?} at {:#06x}: triangle index past the mesh", p.kind, p.offset))),
                Some(WalkmeshFit::Unnamed { .. }) | None => {}
            }
        }
        if literal_placement {
            s.entities_with_literal_placement += 1;
        }
    }
    s.char_entities += char_entities;
    if char_entities == usize::from(layout.header.model_count) {
        s.fields_where_char_entities_equal_model_count += 1;
    }
    *s.fields_by_pc_count.entry(pc_chars.len()).or_default() += 1;
    let distinct: BTreeSet<u8> = pc_chars.iter().copied().collect();
    if distinct.len() != pc_chars.len() {
        s.pc_duplicate_character_in_field += 1;
    }

    // MAPJUMPs
    for j in script.map_jumps() {
        s.map_jump_instructions += 1;
        if let Some(target) = maplist.and_then(|m| m.name(j.field_id)) {
            s.map_jumps_resolved += 1;
            if in_archive.contains(&target.to_ascii_lowercase()) {
                s.map_jumps_target_in_archive += 1;
                jumps_to_check.push((name.to_string(), target.to_string(), j.x, j.y, j.triangle));
            }
        }
    }

    // gateways
    if let (Ok(triggers), Some(maplist)) = (field.triggers(), maplist) {
        for g in resolve_gateways(&triggers, maplist) {
            s.gateways_used += 1;
            match &g.target_field {
                Some(t) => {
                    s.gateways_resolved += 1;
                    if in_archive.contains(&t.to_ascii_lowercase()) {
                        s.gateways_target_in_archive += 1;
                    }
                }
                None => s.gateways_unresolved.push((name.to_string(), g.target_field_id)),
            }
        }
    }
}

impl Survey {
    /// Opcodes (of the 243 assigned) that never occur in any decoded script.
    pub fn unseen_opcodes(&self) -> Vec<(u8, &'static str)> {
        fmv_ff7::script::opcode_name_table().into_iter().filter(|(op, _)| !self.opcode_counts.contains_key(op)).collect()
    }

    /// Compact multi-line summary (no game bytes).
    pub fn summary(&self) -> String {
        use std::fmt::Write;
        let mut o = String::new();
        let w = &mut o;
        let _ = writeln!(w, "fields {} (script parse errors {:?}); header versions {:?}", self.fields, self.fields_with_script_parse_error, self.header_versions);
        let _ = writeln!(w, "string offset == end of scripts in {} fields; script scale == model-loader scale in {}", self.string_offset_is_code_end, self.scale_matches_loader);
        let _ = writeln!(w, "entities {}; entry slots {}; slots outside the code area {}; slots sharing an entry with an earlier slot {}", self.entities, self.slots, self.slots_without_code.len(), self.slots_sharing_an_entry);
        let _ = writeln!(w, "distinct scripts {}; tile their span exactly {}; desyncs {}; instructions {}", self.distinct_scripts, self.tile_exactly, self.desyncs.len(), self.instructions);
        let _ = writeln!(w, "script endings {:?}", self.ends);
        let _ = writeln!(w, "no-terminator scripts: fall into a RET {}, into something else {}", self.falls_through_into_ret, self.falls_through_into_other.len());
        let _ = writeln!(w, "jumps {}: to an instruction {}, to the first byte after the script {} (a RET there: {}), anywhere else {}", self.jumps, self.jumps_to_instruction, self.jumps_to_script_end, self.jumps_to_script_end_ret, self.bad_jumps.len());
        let _ = writeln!(w, "opcodes seen {} of 243; unseen: {:?}", self.opcode_counts.len(), self.unseen_opcodes());
        let _ = writeln!(w, "SPECIAL/KAWAI sub-opcodes seen: {:?}", self.sub_counts.keys().map(|(o, s)| format!("{o:02X}.{s:02X}")).collect::<Vec<_>>());
        let _ = writeln!(w, "instructions with a non-zero unused bank nibble, by opcode: {:?}", self.stray_bank_instructions);
        let _ = writeln!(w, "instructions with a banked (variable) operand: {} opcodes", self.banked_operand_instructions.len());
        let _ = writeln!(w, "script 0: init+main split {} / single part {} (first RET inside a skipped region: {}); interior RET in other slots {}; header blank words non-zero in {}", self.script0_with_main_part, self.script0_without_main_part, self.script0_first_ret_conditional, self.interior_ret_in_other_slots, self.header_blank_nonzero);
        let _ = writeln!(w, "dialog tables ok {} (errors {:?}); first pointer == 2+2D in {}; dialogs {}; unterminated {:?}", self.dialog_tables_ok, self.dialog_table_errors, self.dialog_first_pointer_matches, self.dialogs, self.dialogs_unterminated);
        let _ = writeln!(w, "dialog characters: printable ASCII {}, newlines {}, named control markers {}, unnamed {{XX}} markers {}, other glyphs {}", self.text_ascii, self.text_newlines, self.text_named_markers, self.text_hex_markers, self.text_other_glyphs);
        let _ = writeln!(w, "AKAO blocks {} ({} not tagged AKAO, in fields {:?})", self.akao_blocks, self.non_akao_blocks.len(), self.non_akao_blocks.iter().map(|(f, _)| f.as_str()).collect::<BTreeSet<_>>());
        let _ = writeln!(w, "model loader walked exactly: {}; script model count == loader count: {}", self.model_loader_exact, self.model_count_matches_loader);
        let _ = writeln!(w, "CHAR entities {}; fields where #CHAR entities == model count {}; CHAR operand == model index {} / differs {}; CHAR in conditional code {}", self.char_entities, self.fields_where_char_entities_equal_model_count, self.char_id_equals_model_index, self.char_id_differs_from_model_index, self.chars_with_conditional_char);
        let _ = writeln!(w, "PC entities per field {:?}; PC character ids {:?}; fields binding a character twice {}", self.fields_by_pc_count, self.pc_character_ids, self.pc_duplicate_character_in_field);
        let _ = writeln!(w, "placements {} (conditional {}, in the main part {}, variable operands {}); entities with a literal placement {}", self.placements, self.placements_conditional, self.placements_main_phase, self.placements_variable_operands, self.entities_with_literal_placement);
        let _ = writeln!(w, "placements inside their named triangle {}; outside {}; no such triangle {}; max |height error| {:.2}; over 8 units {}", self.placement_inside, self.placement_outside.len(), self.placement_no_triangle.len(), self.max_abs_height_error, self.height_errors_over_8);
        let _ = writeln!(w, "gateways {}: resolved {} (target in archive {}), unresolved {:?}", self.gateways_used, self.gateways_resolved, self.gateways_target_in_archive, self.gateways_unresolved);
        let _ = writeln!(w, "MAPJUMP (distinct scripts, literal operands) {}: resolved {} (target in archive {}); destination on its triangle {} / (0,0,0) placeholders {} / other {}", self.map_jump_instructions, self.map_jumps_resolved, self.map_jumps_target_in_archive, self.map_jump_destinations_on_triangle, self.map_jump_destinations_zero, self.map_jump_destinations_off_triangle.len());
        o
    }
}
