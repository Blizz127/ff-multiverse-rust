//! Synthetic regressions for authoritative field-point writes; no owned data or store I/O.

use super::{run_panel_exact, run_script_exact, run_talk_exact, ExactBattle, ExactError, ExactRun, ExactScript};
use crate::script::{synth::{dialog, SynthScript}, FieldScript};
use crate::story::{PartyOp, StoryState};

fn set_byte(bank: u8, address: u8, value: u8) -> Vec<u8> {
    vec![0x80, bank << 4, address, value]
}

fn set_word(bank: u8, address: u8, value: u16) -> Vec<u8> {
    let [lo, hi] = value.to_le_bytes();
    vec![0x81, bank << 4, address, lo, hi]
}

fn if_byte(bank: u8, address: u8, op: u8, right: u8, body: &[u8]) -> Vec<u8> {
    let mut code = vec![0x14, bank << 4, address, right, op, u8::try_from(body.len() + 1).unwrap()];
    code.extend_from_slice(body);
    code
}

fn if_word(bank: u8, address: u8, right: u16, body: &[u8]) -> Vec<u8> {
    let [lo, hi] = right.to_le_bytes();
    let mut code = vec![0x18, bank << 4, address, 0, lo, hi, 0, u8::try_from(body.len() + 1).unwrap()];
    code.extend_from_slice(body);
    code
}

fn section(setup: &[Vec<u8>], talk: &[Vec<u8>], dialogs: Vec<Vec<u8>>) -> Vec<u8> {
    let mut init = setup.concat();
    // The first RET ends init, the second ends its main pass. Keep both separate from slot 1.
    init.extend([0, 0]);
    let mut talk = talk.concat();
    talk.push(0);
    SynthScript { dialogs, ..Default::default() }.entity("npc", vec![(0, init), (1, talk)]).build()
}

fn state() -> StoryState { StoryState::at(36).into_extended().unwrap() }

fn run(section: &[u8], state: &StoryState, answers: &[usize]) -> Result<ExactRun, ExactError> {
    run_talk_exact(&FieldScript::parse(section).unwrap(), 0, state, 7, answers)
}

fn writes(run: &ExactRun) -> Vec<(u8, u8, u8, u8)> {
    run.writes.iter().map(|w| (w.bank, w.address, w.value, w.mask)).collect()
}

#[test]
fn exact_writes_and_subsequent_reads_share_both_persistent_bank_views() {
    let code = section(&[], &[
        set_byte(1, 40, 0x12), set_byte(1, 41, 0x34),
        if_word(2, 40, 0x3412, &set_byte(3, 66, 1)),
        set_word(4, 80, 0xABCD),
        if_byte(3, 80, 0, 0xCD, &set_byte(11, 10, 1)),
    ], vec![]);
    assert_eq!(writes(&run(&code, &state(), &[]).unwrap()), vec![
        (1, 40, 0x12, 0xFF), (1, 41, 0x34, 0xFF), (3, 66, 1, 0xFF),
        (3, 80, 0xCD, 0xFF), (3, 81, 0xAB, 0xFF), (11, 10, 1, 0xFF),
    ]);
}

#[test]
fn exact_bit_writes_preserve_unknown_neighbors_and_numeric_reads_do_not_invent_them() {
    let mut before = state();
    before.write_masked(3, 66, 0x80, 0x80).unwrap();
    let bit_on = vec![0x82, 0x30, 66, 1];
    let bit_off = vec![0x83, 0x40, 66, 2];
    let code = section(&[], &[bit_on.clone(), bit_off.clone()], vec![]);
    let result = run(&code, &before, &[]).unwrap();
    assert_eq!(writes(&result), vec![(3, 66, 2, 6)]);
    let mut after = before.clone();
    for w in result.writes { after.write_masked(w.bank, w.address, w.value, w.mask).unwrap(); }
    assert_eq!(after.known_mask(3, 66), 0x86);
    assert_eq!(after.known_bit(3, 66, 7), Some(true));
    assert_eq!(after.known_bit(3, 66, 1), Some(true));
    assert_eq!(after.known_bit(3, 66, 2), Some(false));
    assert_eq!(after.known_bit(3, 66, 3), None);
    let numeric = section(&[], &[bit_on, bit_off, vec![0x85, 0x30, 66, 1]], vec![]);
    assert!(run(&numeric, &before, &[]).is_err(), "PLUS requires the whole prior byte");
    assert_eq!(before.known_mask(3, 66), 0x80, "evaluation never mutates its input");
}

#[test]
fn an_exact_bit_guard_requires_only_the_tested_bit_but_a_numeric_guard_requires_all_bits() {
    let mut before = state();
    before.write_masked(3, 66, 2, 2).unwrap();
    let bit = section(&[], &[if_byte(4, 66, 9, 1, &set_byte(7, 10, 42))], vec![]);
    assert_eq!(writes(&run(&bit, &before, &[]).unwrap()), vec![(7, 10, 42, 0xFF)]);
    let number = section(&[], &[if_byte(4, 66, 0, 2, &set_byte(7, 10, 42))], vec![]);
    assert!(run(&number, &before, &[]).is_err());
    let unknown_bit = section(&[], &[if_byte(4, 66, 10, 2, &set_byte(7, 10, 42))], vec![]);
    assert!(run(&unknown_bit, &before, &[]).is_err(), "unknown does not mean clear");
}

fn question() -> Vec<u8> {
    let mut text = dialog("Choose");
    text.pop();
    text.extend([0xE7, 0xE0]);
    text.extend(dialog("Yes").into_iter().take(3));
    text.extend([0xE7, 0xE0]);
    text.extend(dialog("No"));
    text
}

#[test]
fn exact_questions_require_every_answer_in_range_and_consume_no_trailing_answer() {
    let code = section(&[], &[
        set_byte(5, 19, 1),
        vec![0x48, 5, 0, 0, 1, 2, 19],
        if_byte(5, 19, 0, 2, &set_byte(3, 66, 7)),
    ], vec![question()]);
    assert_eq!(writes(&run(&code, &state(), &[2]).unwrap()), vec![(3, 66, 7, 0xFF)]);
    for answers in [&[][..], &[0][..], &[3][..], &[2, 1][..]] {
        assert!(run(&code, &state(), answers).is_err(), "invalid transcript {answers:?}");
    }
    let plain = section(&[], &[vec![0x40, 0, 0], set_byte(3, 66, 7)], vec![dialog("Hello")]);
    assert!(run(&plain, &state(), &[1]).is_err(), "a plain message cannot consume an answer");
}

#[test]
fn unsupported_resources_and_variable_side_effects_never_leave_a_committable_prefix() {
    for effect in [
        vec![0x39, 0, 1, 0, 0, 0], // GOLDu 1
        vec![0x3A, 0, 1, 0, 0, 0], // GOLDd 1
        vec![0x58, 0, 0, 0, 1], // STITM item 0 x1
        vec![0x3B, 0x33, 66, 67], // CHGLD writes the gil balance into variables
        vec![0xC8, 0], // PRTYP Cloud changes the party
        vec![0x0C], // unassigned opcode
    ] {
        let code = section(&[], &[set_byte(3, 66, 7), effect.clone(), set_byte(3, 67, 8)], vec![]);
        assert!(run(&code, &state(), &[]).is_err(), "unsupported effect {effect:?}");
    }
}

#[test]
fn exact_setup_failures_are_not_erased_when_the_talk_itself_is_simple() {
    for setup in [
        if_byte(3, 200, 0, 0, &set_byte(3, 201, 1)), // unknown setup branch
        vec![0x39, 0, 1, 0, 0, 0], // unsupported setup resource change
        vec![0x48, 5, 0, 0, 1, 2, 19], // setup cannot silently choose an answer
    ] {
        let code = section(std::slice::from_ref(&setup), &[set_byte(3, 202, 99)], vec![question()]);
        assert!(run(&code, &state(), &[]).is_err(), "setup fault {setup:?}");
    }
}

#[test]
fn an_authored_assignment_is_journaled_even_when_its_value_matches_input_or_setup() {
    let mut before = state();
    before.write_byte(3, 66, 7).unwrap();
    let code = section(&[], &[set_byte(4, 66, 7)], vec![]);
    assert_eq!(writes(&run(&code, &before, &[]).unwrap()), vec![(3, 66, 7, 0xFF)]);
    let code = section(&[set_byte(3, 66, 7), set_byte(3, 67, 8)], &[set_byte(4, 66, 7)], vec![]);
    before.write_byte(3, 67, 8).unwrap();
    assert_eq!(writes(&run(&code, &before, &[]).unwrap()), vec![(3, 66, 7, 0xFF)],
        "unchanged setup is an input overlay, but a matching write made by the talk is still authored");
    assert!(run(&code, &state(), &[]).is_err(), "talk-time setup cannot invent new persistent knowledge");
    before.write_byte(3, 67, 9).unwrap();
    assert!(run(&code, &before, &[]).is_err(), "talk-time setup cannot overwrite existing persistent data");
}

#[test]
fn exact_requests_wait_for_the_callee_and_refuse_async_reordering_or_missing_targets() {
    for opcode in [0x01, 0x02, 0x03] {
        let code = SynthScript::default()
            .entity("npc", vec![(0, vec![0, 0]), (1, vec![opcode, 1, 3, 0])])
            .entity("callee", vec![(3, [set_byte(4, 66, 7), vec![0]].concat())]).build();
        let result = run(&code, &state(), &[]);
        if opcode == 0x03 {
            assert_eq!(writes(&result.unwrap()), vec![(3, 66, 7, 0xFF)]);
        } else {
            assert!(result.is_err(), "async request {opcode:#x} cannot be serialized as exact");
        }
    }
    let missing = section(&[], &[vec![0x03, 99, 3], set_byte(3, 66, 7)], vec![]);
    assert!(run(&missing, &state(), &[]).is_err());
}

#[test]
fn exact_words_cannot_wrap_or_partially_write_across_the_bank_boundary() {
    let code = section(&[], &[set_word(4, 255, 0x1234)], vec![]);
    assert!(run(&code, &state(), &[]).is_err());
    let code = section(&[], &[if_word(4, 255, 0, &set_byte(3, 66, 7))], vec![]);
    assert!(run(&code, &state(), &[]).is_err());
}

#[test]
fn an_exact_panel_reads_its_observed_triangle_through_either_bank_view() {
    let branch = if_word(2, 30, 8, &[0x83, 0x30, 66, 1]);
    let main = [vec![0, 0xB9, 0x02, 0, 30], branch, vec![0]].concat();
    let code = SynthScript::default().entity("panel", vec![(0, main)]).build();
    let script = FieldScript::parse(&code).unwrap();
    let mut before = state();
    before.write_masked(3, 66, 2, 2).unwrap();
    assert_eq!(writes(&run_panel_exact(&script, 0, &before, 7, 8, &[(1, 30)]).unwrap()),
        vec![(1, 30, 8, 255), (1, 31, 0, 255), (3, 66, 0, 2)],
        "the authored GETAI assignments are retained along with the bit clear");
    assert!(run_panel_exact(&script, 0, &before, 7, 8, &[(1, 255)]).is_err(),
        "the observed triangle also needs both bytes of one bank");
}

fn script_run(code: &[u8], entry: &ExactScript) -> Result<ExactRun, ExactError> {
    let section = SynthScript::default().entity("entry", vec![(1, code.to_vec())]).build();
    run_script_exact(&FieldScript::parse(&section).unwrap(), &state(), 7, entry, &[])
}

#[test]
fn exact_battle_and_countdown_continuations_require_the_verified_once_only_boundary() {
    let battle = |formation: u16| { let [lo, hi] = formation.to_le_bytes(); vec![0x70, 0, lo, hi] };
    let entry = ExactScript { entity: 0, slot: 1, after_battle: Some(324), countdown: Some(600), ..ExactScript::default() };
    let timer = vec![0x38, 0, 0, 0, 10, 0];
    let good = [battle(324), timer.clone(), set_byte(3, 66, 7), vec![0]].concat();
    assert_eq!(writes(&script_run(&good, &entry).unwrap()), vec![(3, 66, 7, 255)]);
    for code in [
        [battle(303), timer.clone(), vec![0]].concat(),
        [battle(324), battle(324), timer.clone(), vec![0]].concat(),
        [timer.clone(), vec![0]].concat(),
        [battle(324), vec![0]].concat(),
        [battle(324), timer.clone(), timer, vec![0]].concat(),
    ] { assert!(script_run(&code, &entry).is_err(), "wrong, absent or repeated external boundary"); }
    assert!(script_run(&good, &ExactScript { entity: 0, slot: 1, ..ExactScript::default() }).is_err());
}

#[test]
fn exact_main_setup_audits_only_init_and_keeps_scratch_without_committing_it() {
    let init = [set_byte(5, 20, 9), vec![0], set_byte(3, 99, 88), vec![0]].concat();
    let root = [if_byte(6, 20, 0, 9, &set_byte(3, 66, 7)), vec![0]].concat();
    let section = SynthScript::default().entity("entry", vec![(0, init), (1, root)]).build();
    let script = FieldScript::parse(&section).unwrap();
    let entry = ExactScript { entity: 0, slot: 1, setup: true, ..ExactScript::default() };
    assert_eq!(writes(&run_script_exact(&script, &state(), 7, &entry, &[]).unwrap()), vec![(3, 66, 7, 255)],
        "the unrelated main write must not run during init-only setup");
}

#[test]
fn exact_idle_completion_is_distinct_from_an_active_loop_or_malformed_tail() {
    let entry = ExactScript { entity: 0, slot: 1, ..ExactScript::default() };
    let idle = [set_byte(3, 66, 7), vec![0x12, 0]].concat();
    assert_eq!(writes(&script_run(&idle, &entry).unwrap()), vec![(3, 66, 7, 255)]);
    // The temporary byte changes every lap; it never becomes an idle completion.
    assert_eq!(script_run(&[0x95, 0x05, 20, 0x12, 3], &entry), Err(ExactError::End(super::End::Budget)));
    assert!(script_run(&[set_byte(3, 66, 7), vec![0x0C]].concat(), &entry).is_err());
}

#[test]
fn exact_getai_requires_the_specific_observed_word_and_persists_only_authored_destinations() {
    let code = [vec![0xB9, 0x06, 0, 30], if_word(5, 30, 8, &set_byte(3, 66, 7)), vec![0]].concat();
    let mut entry = ExactScript { entity: 0, slot: 1, ..ExactScript::default() };
    assert!(script_run(&code, &entry).is_err());
    entry.seeds = vec![((5, 30), 8), ((5, 31), 0)];
    entry.triangle_sources = vec![(5, 30, 0)];
    assert_eq!(writes(&script_run(&code, &entry).unwrap()), vec![(3, 66, 7, 255)]);
}

#[test]
fn only_a_proven_completion_join_can_serialize_an_async_scratch_write() {
    // REQ child; IFUB temp6 ==1 { JMPF exit }; camera-only wait loop; exit writes story.
    let call = vec![0x01, 1, 3];
    let guard = vec![0x14, 0x50, 6, 1, 0, 3];
    let jump_exit = vec![0x10, 6];
    let wait_loop = vec![0x24, 1, 0, 0x12, 11];
    let caller = [call, guard, jump_exit, wait_loop, set_byte(3, 66, 7), vec![0]].concat();
    let make = |child: Vec<u8>| SynthScript::default()
        .entity("entry", vec![(0, vec![0, 0]), (1, caller.clone())])
        .entity("child", vec![(3, child)]).build();
    let section = make([vec![0x24, 1, 0], set_byte(5, 6, 1), vec![0]].concat());
    assert_eq!(writes(&run(&section, &state(), &[]).unwrap()), vec![(3, 66, 7, 255)]);
    let section = make([set_byte(3, 70, 9), set_byte(5, 6, 1), vec![0]].concat());
    assert!(run(&section, &state(), &[]).is_err(), "a joined child may not hide persistent state effects");
    let section = make([set_byte(5, 6, 1), vec![0x24, 1, 0, 0]].concat());
    assert!(run(&section, &state(), &[]).is_err(), "the signal must be the child's final instruction before RET");
}

#[test]
fn bounded_async_actor_scripts_may_run_when_no_position_is_read_back() {
    let make = |child: Vec<u8>| SynthScript { dialogs: vec![dialog("Hello")], ..Default::default() }
        .entity("entry", vec![(0, vec![0, 0]), (1, [vec![0x01, 1, 3], set_byte(3, 66, 7), vec![0]].concat())])
        .entity("child", vec![(3, child)]).build();
    assert_eq!(writes(&run(&make(vec![0x40, 0, 0, 0]), &state(), &[]).unwrap()), vec![(3, 66, 7, 255)]);
    assert_eq!(writes(&run(&make(vec![0xA8, 0, 1, 0, 2, 0, 0]), &state(), &[]).unwrap()), vec![(3, 66, 7, 255)]);
}

#[test]
fn exact_byte_extracts_use_the_original_source_width_and_destination_address() {
    let mut before = state();
    before.write_byte(3, 80, 0xCD).unwrap();
    before.write_byte(3, 91, 0xAB).unwrap();
    let code = section(&[], &[
        vec![0x9A, 0x53, 10, 80], // LBYTE temp10 = byte3.80; 3.81 remains unknown.
        if_byte(5, 10, 0, 0xCD, &set_byte(3, 66, 1)),
        vec![0x9B, 0x34, 70, 90, 0], // HBYTE 3.70 reads only known high byte4.91;4.90 is unknown.
        vec![0x9B, 0x30, 255, 0x34, 0x12], // The stated destination255 is valid.
    ], vec![]);
    assert_eq!(writes(&run(&code, &before, &[]).unwrap()), vec![(3, 66, 1, 255), (3, 70, 0xAB, 255), (3, 255, 0x12, 255)]);
    for source in [92, 255] {
        let code = section(&[], &[vec![0x9B, 0x34, 70, source, 0]], vec![]);
        assert!(run(&code, &before, &[]).is_err(), "unknown selected byte or cross-bank source must refuse");
    }
    let code = section(&[], &[vec![0x82, 0x30, 70, 8]], vec![]);
    assert!(run(&code, &before, &[]).is_err(), "unsupported bit index never redirects a write into the next byte");
    let code = section(&[], &[vec![0x9C, 0x30, 0, 70, 1, 2]], vec![]);
    assert!(run(&code, &before, &[]).is_err(), "2BYTE bank decoding needs its own correction before exact use");
}


#[test]
fn exact_party_assignments_journal_resolved_slots_and_reject_malformed_or_unrepresentable_parties() {
    let mut before = StoryState::at(10).into_extended().unwrap();
    for empty in [254, 255] {
        let code = section(&[], &[vec![0xCA, 0, 1, empty], set_byte(3, 66, 7)], vec![]);
        if empty == 255 {
            assert!(matches!(run(&code, &before, &[]), Err(ExactError::UnknownRead { .. })), "255 cannot retain unobserved raw slots");
            for (address, value) in [(9, 0), (10, 255), (11, 255)] { before.write_byte(3, address, value).unwrap(); }
        }
        let result = run(&code, &before, &[]).unwrap();
        assert_eq!(writes(&result), vec![(3, 9, 0, 255), (3, 10, 1, 255), (3, 11, 255, 255), (3, 66, 7, 255)]);
        assert_eq!(result.party, vec![PartyOp::Set([0, 1, 254])], "Barret's authored join is explicit");
    }
    for party in [[0, 254, 1], [0, 0, 254], [0, 1, 9], [0, 1, 253]] {
        let code = section(&[], &[set_byte(3, 66, 7), [vec![0xCA], party.to_vec()].concat()], vec![]);
        assert!(run(&code, &before, &[]).is_err(), "a prefix cannot escape after malformed party {party:?}");
    }
}

#[test]
fn exact_party_fill_and_queries_use_the_current_overlay_until_the_next_assignment() {
    let before = StoryState::at(10).into_extended().unwrap();
    let body = set_byte(3, 74, 1);
    let member = [vec![0xCB, 1, u8::try_from(body.len() + 1).unwrap()], body].concat();
    let code = section(&[], &[
        vec![0xCA, 0, 1, 254], // Add Barret explicitly; these are now proven raw bytes.
        vec![0xCA, 0, 255, 254], // Original255 retains that raw Barret slot.
        vec![0x74, 3, 0, 70], vec![0x74, 3, 1, 71], vec![0x74, 3, 2, 72], member,
        vec![0xCA, 0, 254, 254], vec![0x74, 3, 1, 73],
        vec![0xCB, 1, 5], set_byte(3, 74, 2),
    ], vec![]);
    let result = run(&code, &before, &[]).unwrap();
    assert_eq!(result.party, vec![PartyOp::Set([0, 1, 254]), PartyOp::Set([0, 1, 254]), PartyOp::Set([0, 254, 254])]);
    assert_eq!(writes(&result), vec![(3, 9, 0, 255), (3, 10, 255, 255), (3, 11, 255, 255), (3, 70, 0, 255), (3, 71, 1, 255), (3, 72, 255, 255), (3, 73, 255, 255), (3, 74, 1, 255)]);
}

#[test]
fn exact_setup_party_changes_cannot_be_replayed_as_new_field_initialization() {
    let unchanged = section(&[vec![0xCA, 0, 254, 254]], &[set_byte(3, 66, 7)], vec![]);
    assert!(matches!(run(&unchanged, &state(), &[]), Err(ExactError::SetupWrite { .. })), "setup cannot invent missing party bytes");
    let mut established = state();
    for (address, value) in [(9, 0), (10, 255), (11, 255)] { established.write_byte(3, address, value).unwrap(); }
    assert!(run(&unchanged, &established, &[]).is_ok());
    for setup in [vec![vec![0xCA, 0, 1, 254]], vec![vec![0xCA, 0, 1, 254], vec![0xCA, 0, 254, 254]]] {
        let code = section(&setup, &[set_byte(3, 66, 7)], vec![]);
        assert_eq!(run(&code, &state(), &[]), Err(ExactError::SetupParty));
    }
}

#[test]
fn exact_battle_mode_is_captured_at_battle_and_setup_mode_survives_into_the_root() {
    let battle = vec![0x70, 0, 44, 1]; // Formation300.
    let entry = ExactScript { entity: 0, slot: 1, after_battle: Some(300), ..ExactScript::default() };
    let default = [battle.clone(), vec![0]].concat();
    assert_eq!(script_run(&default, &entry).unwrap().battle, Some(ExactBattle { formation: 300, flags: 0 }));
    let mode = [vec![0x72, 32, 0], battle.clone(), vec![0x72, 34, 0, 0]].concat();
    assert_eq!(script_run(&mode, &entry).unwrap().battle, Some(ExactBattle { formation: 300, flags: 32 }));
    let section = SynthScript::default()
        .entity("entry", vec![(0, vec![0x72, 32, 0, 0]), (1, [battle, vec![0x72, 34, 0, 0]].concat())]).build();
    let result = run_script_exact(&FieldScript::parse(&section).unwrap(), &state(), 7,
        &ExactScript { setup: true, ..entry }, &[]).unwrap();
    assert_eq!(result.battle, Some(ExactBattle { formation: 300, flags: 32 }), "post-win34 is not retroactive");
}

#[test]
fn triangle_snapshots_are_bound_to_the_observed_actor_and_expire_after_movement() {
    let entry = ExactScript { entity: 0, slot: 1, seeds: vec![((5, 30), 8), ((5, 31), 0)],
        triangle_sources: vec![(6, 30, 0)], ..ExactScript::default() };
    let get = |entity| vec![0xB9, 0x06, entity, 30];
    let result = [get(0), set_byte(3, 66, 7), vec![0]].concat();
    assert!(script_run(&result, &entry).is_ok());
    assert!(script_run(&[get(1), vec![0]].concat(), &entry).is_err(), "another actor cannot reuse the same destination seed");
    let movement = vec![0xA8, 0, 1, 0, 2, 0];
    assert!(script_run(&[movement.clone(), get(0), vec![0]].concat(), &entry).is_err(), "ordinary movement also expires the observation");
    for request in [0x01, 0x03] {
        let make = |observer| SynthScript::default()
            .entity("entry", vec![(1, [vec![request, 1, 3], get(observer), set_byte(3, 66, 7), vec![0]].concat())])
            .entity("moving", vec![(3, [movement.clone(), vec![0]].concat())]).build();
        let code = make(0);
        assert!(run_script_exact(&FieldScript::parse(&code).unwrap(), &state(), 7, &entry, &[]).is_ok(), "moving actor1 does not stale actor0");
        let code = make(1);
        let other = ExactScript { triangle_sources: vec![(5, 30, 1)], ..entry.clone() };
        assert!(run_script_exact(&FieldScript::parse(&code).unwrap(), &state(), 7, &other, &[]).is_err(), "an async/awaited actor cannot use an earlier observation");
    }
    let read_back = vec![0xC1, 0x55, 0x55, 0, 10, 12, 14, 30, 0];
    assert!(script_run(&read_back, &entry).is_err(), "AXYZI has no exact position provenance");
}

#[test]
fn async_callee_visibility_cannot_hide_the_root_conversation() {
    let section = SynthScript::default()
        .entity("entry", vec![(0, vec![0x01, 1, 3, 0, 0]), (1, [set_byte(3, 66, 7), vec![0]].concat())])
        .entity("callee", vec![(3, vec![0xA4, 0, 0x7E, 1, 0])]).build();
    assert_eq!(writes(&run(&section, &state(), &[]).unwrap()), vec![(3, 66, 7, 255)]);
}


#[test]
fn exact_control_can_only_reassert_the_uniquely_observed_trigger_actor() {
    let entry = ExactScript { entity: 0, slot: 1, seeds: vec![((5, 30), 8), ((5, 31), 0)],
        triangle_sources: vec![(5, 30, 0)], ..ExactScript::default() };
    assert!(script_run(&[0xBF, 0, 0], &entry).is_ok());
    assert!(script_run(&[0xBF, 1, 0], &entry).is_err(), "control cannot be transferred to a different entity");
    assert!(script_run(&[0xBF, 0, 0], &ExactScript { entity: 0, slot: 1, ..ExactScript::default() }).is_err(),
        "an unobserved control actor cannot be guessed");
}


fn completed_battle_loop(tail: Vec<u8>, persistent: bool) -> (Vec<u8>, ExactScript) {
    let bank = if persistent { 3 } else { 6 };
    let get = vec![0xB9, bank, 0, 30];
    let action = [vec![0x70, 0, 68, 1], vec![0xA8, 0, 1, 0, 2, 0], set_word(2, 0, 27)].concat();
    let mut before_win = vec![0x16, 0x20, 0, 0, 27, 0, 3, u8::try_from(action.len() + 1).unwrap()];
    before_win.extend(action);
    let guarded = if_word(bank, 30, 8, &[before_win, tail].concat());
    let mut root = [get, guarded].concat();
    let distance = u8::try_from(root.len()).unwrap();
    root.extend([0x12, distance]);
    let section = SynthScript::default().entity("entry", vec![(1, root)]).build();
    let entry = ExactScript { entity: 0, slot: 1, after_battle: Some(324),
        seeds: vec![((bank, 30), 8), ((bank, 31), 0)], triangle_sources: vec![(bank, 30, 0)], ..ExactScript::default() };
    (section, entry)
}

fn run_completed_loop(section: &[u8], entry: &ExactScript) -> Result<ExactRun, ExactError> {
    let before = StoryState::at(26).into_extended().unwrap();
    run_script_exact(&FieldScript::parse(section).unwrap(), &before, 7, entry, &[])
}

#[test]
fn a_completed_battle_can_finish_only_after_proving_every_fresh_triangle_branch_idle() {
    // Guard shape: triangle==8, moment<27 => battle and moment27; moment==123 => future action.
    // On returning after victory neither moment guard holds, regardless of the fresh triangle.
    let tail = if_word(2, 0, 123, &set_byte(3, 70, 1));
    let (section, entry) = completed_battle_loop(tail, false);
    let result = run_completed_loop(&section, &entry).unwrap();
    assert_eq!(result.battle, Some(ExactBattle { formation: 324, flags: 0 }));
    assert_eq!(writes(&result), vec![(1, 0, 27, 255), (1, 1, 0, 255)]);
    let (section, entry) = completed_battle_loop(vec![], true);
    assert!(run_completed_loop(&section, &entry).is_err(), "a refreshed persistent GETAI is itself an effect");
}

#[test]
fn a_completed_battle_cannot_hide_future_writes_requests_or_a_different_idle_cycle() {
    for effect in [set_byte(3, 70, 1), vec![0x03, 0, 2], vec![0x70, 0, 68, 1], vec![0], vec![0x12, 0]] {
        // The first real observation is8, so none of these instructions runs before the
        // post-win backedge. A fresh triangle9 can reach them; the idle proof must inspect it.
        let action = [vec![0x70, 0, 68, 1], vec![0xA8, 0, 1, 0, 2, 0], set_word(2, 0, 27)].concat();
        let mut root = [vec![0xB9, 6, 0, 30], if_word(2, 0, 26, &action), if_word(5, 30, 9, &effect)].concat();
        let back = u8::try_from(root.len()).unwrap();
        root.extend([0x12, back]);
        let section = SynthScript::default().entity("entry", vec![(1, root)]).build();
        let entry = ExactScript { entity: 0, slot: 1, after_battle: Some(324),
            seeds: vec![((5, 30), 8), ((5, 31), 0)], triangle_sources: vec![(5, 30, 0)], ..ExactScript::default() };
        assert!(run_completed_loop(&section, &entry).is_err(), "future effect/cycle is not an idle boundary: {effect:?}");
    }
    // A RET actually reached during ordinary execution is a valid ending, not a speculative
    // path through a fresh observation. The special idle proof is not involved in that case.
    let (section, entry) = completed_battle_loop(if_word(2, 0, 27, &[0]), false);
    assert!(run_completed_loop(&section, &entry).is_ok());
    // The comparison would consume a genuinely unknown independent persistent byte.
    let (section, entry) = completed_battle_loop(if_byte(3, 70, 0, 1, &set_byte(3, 71, 1)), false);
    assert!(run_completed_loop(&section, &entry).is_err());
}

#[test]
fn an_idle_proof_treats_alias_high_byte_and_overlapping_reads_as_fresh_observations() {
    let effect = set_byte(3, 70, 1);
    for hidden in [
        if_byte(5, 30, 0, 9, &effect), // Low byte: stale value8 would hide this write.
        if_byte(5, 31, 0, 1, &effect), // High byte through the alias: stale value0 would hide it.
        if_word(5, 29, 0x0900, &effect), // The word overlaps only the low triangle byte.
    ] {
        let (section, entry) = completed_battle_loop(hidden, false);
        assert!(run_completed_loop(&section, &entry).is_err(), "both possibilities of every overlapping observation must be proved");
    }
}

#[test]
fn an_idle_proof_requires_a_prior_root_observation_and_a_completed_expected_battle() {
    let entry = ExactScript { entity: 0, slot: 1, seeds: vec![((5, 30), 8), ((5, 31), 0)],
        triangle_sources: vec![(5, 30, 0)], ..ExactScript::default() };
    let get = vec![0xB9, 6, 0, 30];
    let movement = vec![0xA8, 0, 1, 0, 2, 0];
    let mut no_battle = [get.clone(), movement.clone()].concat();
    no_battle.extend([0x12, u8::try_from(no_battle.len()).unwrap()]);
    assert!(script_run(&no_battle, &entry).is_err(), "generic unchanged-loop detection cannot bypass a stale GETAI");
    let first_read = [vec![0x70, 0, 68, 1], movement, get, vec![0x12, 4]].concat();
    assert!(script_run(&first_read, &ExactScript { after_battle: Some(324), ..entry }).is_err(), "first read has no observed-action boundary");
}


#[test]
fn position_taint_invalidates_idle_labels_before_the_next_observation() {
    let get = vec![0xB9, 6, 0, 30];
    let prefix = [get.clone(), vec![0x70, 0, 68, 1], set_word(2, 0, 27)].concat();
    let label = prefix.len();
    let movement = vec![0xA8, 0, 1, 0, 2, 0];
    // P: JMP G; G: GETAI; if triangle==8 { MOVE; JMP P } else { persistent_write; RET }.
    let jump_back_at = label + 2 + get.len() + 8 + movement.len();
    let body = [movement, vec![0x12, u8::try_from(jump_back_at - label).unwrap()]].concat();
    let code = [prefix, vec![0x10, 1], get, if_word(5, 30, 8, &body), set_byte(3, 99, 1), vec![0]].concat();
    let entry = ExactScript { entity: 0, slot: 1, after_battle: Some(324),
        seeds: vec![((5, 30), 8), ((5, 31), 0)], triangle_sources: vec![(5, 30, 0)], ..ExactScript::default() };
    assert!(script_run(&code, &entry).is_err(), "repeated P cannot finish before the now-symbolic triangle branch is proved safe");
}


#[test]
fn authored_party_replacement_updates_mirrors_but_never_initializes_them_by_inference() {
    let mut before = StoryState::at(11).into_extended().unwrap();
    let read = vec![0x80, 0x54, 20, 10];
    let code = section(&[], std::slice::from_ref(&read), vec![]);
    assert!(matches!(run(&code, &before, &[]), Err(ExactError::UnknownRead { bank: 3, address: 10, .. })),
        "a canonical party is not evidence that an untouched persistent byte is known");
    before.write_byte(3, 9, 0).unwrap();
    before.write_byte(3, 10, 1).unwrap();
    before.write_byte(3, 11, 255).unwrap();
    let code = section(&[], &[
        vec![0xCA, 2, 254, 254], read,
        if_byte(5, 20, 0, 255, &set_byte(3, 70, 1)),
        if_byte(4, 9, 0, 2, &set_byte(3, 71, 1)),
    ], vec![]);
    let result = run(&code, &before, &[]).unwrap();
    assert_eq!(writes(&result), vec![(3, 9, 2, 255), (3, 10, 255, 255), (3, 11, 255, 255), (3, 70, 1, 255), (3, 71, 1, 255)]);
    let code = section(&[], &[vec![0xCA, 2, 254, 254], set_byte(4, 10, 2)], vec![]);
    assert_eq!(run(&code, &before, &[]), Err(ExactError::PartyMirror { address: 10 }), "a direct write cannot silently bypass GETPC's overlay");
}


#[test]
fn party_reordering_cannot_hide_wrong_intermediate_getpc_results_by_restoring_the_final_order() {
    let before = StoryState::at(11).into_extended().unwrap();
    for reordered in [[1, 0, 254], [1, 254, 254]] {
        let code = section(&[], &[
            [vec![0xCA], reordered.to_vec()].concat(), vec![0x74, 3, 0, 70], vec![0xCA, 0, 1, 254],
        ], vec![]);
        assert!(run(&code, &before, &[]).is_err(), "saved-party slots differ from bank slots after {reordered:?}");
    }
}

#[test]
fn completed_action_idle_proof_rejects_invalid_bit_indices() {
    let (section, entry) = completed_battle_loop(if_byte(5, 30, 9, 8, &set_byte(3, 70, 1)), false);
    assert!(run_completed_loop(&section, &entry).is_err(), "the original byte condition cannot shift by an out-of-range bit");
}
