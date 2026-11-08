//! Checks of the field script decoder against the owner's REAL FF7 data (FF7 Steam Edition,
//! English; verified 2026-11-08).
//!
//! Every test is `#[ignore]` and also returns early when `FMV_FF7_DATA` is unset, so a normal
//! `cargo test` never touches game data. Run them with
//!
//! ```text
//! FMV_FF7_DATA=<.../data> cargo test -p fmv-ff7 --test script_real_data -j8 -- --ignored
//! ```
//!
//! `FMV_FF7_DATA` is the game's `data/` directory (the one containing `field/flevel.lgp`). The
//! files are only ever opened read-only, and the assertions record counts, offsets and
//! structural properties: no game bytes and no game text are stored in this repository.

mod script_common;

use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::script::{check_placement, parse_model_list, resolve_gateways, resolve_map_jumps, Arg, FieldScript, Maplist, WalkmeshFit};
use script_common::{data_dir, flevel_path, load_field, survey_all};

fn open() -> Option<LgpArchive<std::fs::File>> {
    let Some(dir) = data_dir() else {
        eprintln!("FMV_FF7_DATA is not set; skipping");
        return None;
    };
    Some(LgpArchive::open_path(flevel_path(&dir)).unwrap())
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn every_script_of_every_field_decodes_to_its_end() {
    let Some(mut lgp) = open() else { return };
    let s = survey_all(&mut lgp);
    println!("{}", s.summary());

    // container
    assert_eq!(s.fields, 702);
    assert!(s.fields_with_script_parse_error.is_empty(), "{:?}", s.fields_with_script_parse_error);
    assert_eq!(s.header_versions.iter().map(|(k, v)| (*k, *v)).collect::<Vec<_>>(), [((2, 5), 702)]);
    assert_eq!(s.string_offset_is_code_end, 702);
    assert_eq!(s.scale_matches_loader, 702);
    assert_eq!(s.header_blank_nonzero, 0);
    assert_eq!((s.entities, s.slots), (10_534, 337_088));
    // 29 empty slots of one entity of `snw_w` point at the dialog table, one byte past the code
    assert_eq!(s.slots_without_code.len(), 29);
    assert!(s.slots_without_code.iter().all(|p| p.field == "snw_w" && p.entity == 12));

    // zero desyncs: every script decodes instruction by instruction exactly to its end
    assert_eq!(s.distinct_scripts, 48_087);
    assert_eq!(s.tile_exactly, 48_087);
    assert!(s.desyncs.is_empty(), "{:?}", s.desyncs.iter().take(5).collect::<Vec<_>>());
    assert_eq!(s.instructions, 343_587);
    let ends: Vec<(&str, usize)> = s.ends.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    assert_eq!(
        ends,
        [("FallsThroughIntoOther", 26), ("FallsThroughIntoRet", 947), ("ForwardJump", 7), ("Loop", 1589), ("Ret", 45_509), ("Retto", 9)]
    );

    // every jump lands on an instruction start or on the first byte after its script
    assert_eq!(s.jumps, 40_940);
    assert_eq!(s.jumps_to_instruction, 40_321);
    assert_eq!((s.jumps_to_script_end, s.jumps_to_script_end_ret), (619, 612));
    assert!(s.bad_jumps.is_empty(), "{:?}", s.bad_jumps.iter().take(5).collect::<Vec<_>>());

    // opcode coverage and the bank layout check
    assert_eq!(s.opcode_counts.len(), 222);
    let unseen: Vec<&str> = s.unseen_opcodes().into_iter().map(|(_, m)| m).collect();
    assert_eq!(
        unseen,
        [
            "HMPMAX1", "HMPMAX2", "WROW", "GWCOL", "CMTRA", "NOP", "SCRLO", "MINUS2!", "DEC2!", "BITXOR", "XOR2", "LBYTE", "HBYTE", "SETX", "GETX",
            "SEARCHX", "SLDR2", "PMJMP2", "RTPAL2", "MUSVM", "CHMPH"
        ]
    );
    assert_eq!(s.sub_counts.keys().filter(|(op, _)| *op == 0x0F).count(), 9);
    assert_eq!(s.sub_counts.keys().filter(|(op, _)| *op == 0x28).count(), 13);
    assert!(s.stray_bank_instructions.is_empty(), "{:?}", s.stray_bank_instructions);
    assert_eq!(s.banked_operand_instructions.len(), 89);

    // script 0 is an init part and a main part (split at the first RET)
    assert_eq!((s.script0_with_main_part, s.script0_without_main_part, s.script0_first_ret_conditional), (10_516, 17, 1));

    // dialogs and AKAO
    assert_eq!((s.dialog_tables_ok, s.dialog_first_pointer_matches), (702, 702));
    assert!(s.dialog_table_errors.is_empty());
    assert_eq!(s.dialogs, 56_048);
    assert_eq!(s.dialogs_unterminated, ["life #0", "life2 #0", "mds7pb_2 #49"]);
    // the decoded text reads as text: 2.5 million printable ASCII characters, ninety thousand
    // line breaks, ninety-eight thousand named control markers, and only a few hundred bytes
    // (arguments of control codes, a handful of glyph-less codes) left as raw `{XX}`
    assert!(s.text_ascii > 2_400_000, "{}", s.text_ascii);
    assert!(s.text_newlines > 80_000 && s.text_named_markers > 90_000);
    assert!(s.text_hex_markers * 1000 < s.text_ascii, "{}", s.text_hex_markers);
    assert!(s.text_other_glyphs * 10 < s.text_ascii, "{}", s.text_other_glyphs);
    assert_eq!(s.akao_blocks, 1342);
    assert_eq!(s.non_akao_blocks.len(), 22); // tutorial blocks in six fields

    // models and characters
    assert_eq!((s.model_loader_exact, s.model_count_matches_loader), (702, 702));
    assert_eq!(s.char_entities, 5454);
    assert_eq!(s.fields_where_char_entities_equal_model_count, 702);
    assert_eq!((s.char_id_equals_model_index, s.char_id_differs_from_model_index), (5454, 0));
    assert_eq!(s.chars_with_conditional_char, 0);

    // player binding: up to ten playable-character entities per field
    assert_eq!(s.fields_by_pc_count.get(&0), Some(&38));
    assert_eq!(s.pc_character_ids.keys().copied().max(), Some(10));
    assert_eq!(s.pc_duplicate_character_in_field, 13);

    // placements: 99.5% of the literal ones sit inside the triangle they name
    assert_eq!(s.placements, 3014);
    assert_eq!((s.placements_conditional, s.placements_main_phase, s.placements_variable_operands), (1340, 507, 19));
    assert_eq!((s.placement_inside, s.placement_outside.len(), s.placement_no_triangle.len()), (2978, 15, 1));
    assert!(s.max_abs_height_error < 20.0, "{}", s.max_abs_height_error);
    assert_eq!(s.height_errors_over_8, 6);

    // exits
    assert_eq!((s.gateways_used, s.gateways_resolved, s.gateways_target_in_archive), (1036, 1036, 978));
    assert!(s.gateways_unresolved.is_empty());
    assert_eq!((s.map_jump_instructions, s.map_jumps_resolved, s.map_jumps_target_in_archive), (1801, 1801, 1754));
    assert_eq!(s.map_jump_destinations_on_triangle, 1383);
    assert_eq!(s.map_jump_destinations_zero, 315);
    assert_eq!(s.map_jump_destinations_off_triangle.len(), 56);
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn maplist_names_the_gateway_targets() {
    let Some(mut lgp) = open() else { return };
    let maplist = Maplist::from_archive(&mut lgp).unwrap();
    assert_eq!(maplist.len(), 788);
    // md1stin's only gateway targets id 117 (see `real_data.rs`); the ids around it are its neighbours
    assert_eq!(maplist.name(117), Some("md1_1"));
    assert_eq!(maplist.id_of("md1stin"), Some(116));
    assert_eq!(maplist.id_of("MD1_2"), Some(118));
    assert_eq!(maplist.name(788), None);
}

/// Shared checks for the three fields the task names.
struct Fixture {
    name: &'static str,
    entities: usize,
    models: usize,
    players: &'static [(usize, u8)],
    gateway_targets: &'static [&'static str],
    map_jump_targets: &'static [&'static str],
}

fn check_field(lgp: &mut LgpArchive<std::fs::File>, maplist: &Maplist, f: &Fixture) {
    let field = load_field(lgp, f.name).unwrap();
    let script = FieldScript::from_field(&field).unwrap();
    assert_eq!(script.entity_count(), f.entities, "{}", f.name);
    assert_eq!(usize::from(script.header().model_count), f.models, "{}", f.name);
    let models = parse_model_list(field.section(2).unwrap()).unwrap();
    assert_eq!(models.models.len(), f.models);
    let mesh = field.walkmesh().unwrap();

    let inits = script.entity_inits(Some(&models));
    let chars: Vec<_> = inits.iter().filter(|e| e.is_character()).collect();
    assert_eq!(chars.len(), f.models, "{}: one CHAR entity per model-loader record", f.name);
    for (i, c) in chars.iter().enumerate() {
        assert_eq!(c.model_index, Some(i));
        assert!(c.model_ref.is_some(), "{}: model {i} is in the loader", f.name);
        assert_eq!(c.char_id.map(|t| t.value), Some(Arg::Literal(u8::try_from(i).unwrap())));
        assert_eq!(c.decode_error, None);
    }
    assert_eq!(script.player_entities(), f.players, "{}", f.name);

    // every literal placement falls on the walkmesh triangle it names, at the triangle's height
    let mut placed = 0;
    for e in &inits {
        for p in &e.placements {
            match check_placement(&mesh, p) {
                Some(WalkmeshFit::Inside { height_error }) => {
                    placed += 1;
                    assert!(height_error.is_none_or(|h| h.abs() < 2.0), "{} entity {} height error {height_error:?}", f.name, e.name);
                }
                other => panic!("{} entity {}: {other:?}", f.name, e.name),
            }
        }
    }
    assert!(placed >= 5, "{}: only {placed} placements", f.name);

    // gateways and MAPJUMPs carry maplist names
    let triggers = field.triggers().unwrap();
    let gateways = resolve_gateways(&triggers, maplist);
    let names: Vec<&str> = gateways.iter().filter_map(|g| g.target_field.as_deref()).collect();
    assert_eq!(names, f.gateway_targets, "{}", f.name);
    for g in &gateways {
        assert!(g.destination_triangle.is_some());
    }
    let jumps = resolve_map_jumps(&script.map_jumps(), maplist);
    let names: Vec<&str> = jumps.iter().filter_map(|j| j.target_field.as_deref()).collect();
    assert_eq!(names, f.map_jump_targets, "{}", f.name);

    // the dialogs decode and end properly
    assert!(script.dialog_error().is_none());
    for d in script.dialogs() {
        assert!(d.terminated, "{} dialog {}", f.name, d.index);
        assert!(script.dialog_text(d.index).is_some());
    }
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn md1stin_md1_1_and_mds7st1_npcs_and_exits() {
    let Some(mut lgp) = open() else { return };
    let maplist = Maplist::from_archive(&mut lgp).unwrap();
    let fixtures = [
        Fixture { name: "md1stin", entities: 12, models: 10, players: &[(1, 0), (2, 1)], gateway_targets: &["md1_1"], map_jump_targets: &[] },
        Fixture {
            name: "md1_1",
            entities: 12,
            models: 7,
            players: &[(1, 0), (2, 1)],
            gateway_targets: &["md1stin", "md1_2"],
            map_jump_targets: &["md1_2"],
        },
        Fixture {
            name: "mds7st1",
            entities: 23,
            models: 10,
            players: &[(12, 0), (13, 3), (14, 2), (15, 8)],
            gateway_targets: &["mds7st2"],
            map_jump_targets: &["colne_b3"],
        },
    ];
    for f in &fixtures {
        check_field(&mut lgp, &maplist, f);
    }

    // md1stin: the player entity of character 0 is placed by script 0's main part, only when a
    // story variable says so; its gateway arrival point lies inside the named target triangle
    let field = load_field(&mut lgp, "md1stin").unwrap();
    let script = FieldScript::from_field(&field).unwrap();
    let inits = script.entity_inits(None);
    let cloud = inits.iter().find(|e| Some(e.entity) == script.player_entity(0)).unwrap();
    let p = cloud.placement().unwrap();
    assert!(p.conditional);
    assert_eq!(p.phase, fmv_ff7::script::Phase::Main);
    let triggers = field.triggers().unwrap();
    let g = &resolve_gateways(&triggers, &maplist)[0];
    let target = load_field(&mut lgp, g.target_field.as_deref().unwrap()).unwrap().walkmesh().unwrap();
    let tri = target.triangles.get(g.destination_triangle.unwrap()).unwrap();
    assert!(fmv_ff7::script::triangle_contains_xy(tri, i32::from(g.destination_x), i32::from(g.destination_y)));
}
