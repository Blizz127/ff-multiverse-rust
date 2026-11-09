//! Tests of the talk walker on synthetic scripts (never real game data).

use super::*;
use crate::script::synth::{dialog, SynthScript};

// ---- a tiny assembler ----------------------------------------------------------------------

const RET: u8 = 0x00;

fn message(window: u8, dialog: u8) -> Vec<u8> {
    vec![0x40, window, dialog]
}

/// `ASK window dialog first last var` with the answer variable `Var[bank][addr]`.
fn ask(dialog: u8, first: u8, last: u8, bank: u8, addr: u8) -> Vec<u8> {
    vec![0x48, bank, 0, dialog, first, last, addr]
}

/// `SETBYTE Var[bank][addr] = value`.
fn setbyte(bank: u8, addr: u8, value: u8) -> Vec<u8> {
    vec![0x80, bank << 4, addr, value]
}

/// `IFUB Var[bank][addr] <op> right` guarding `body` (the jump skips exactly the body).
fn if_ub(bank: u8, addr: u8, op: u8, right: u8, body: &[u8]) -> Vec<u8> {
    let mut v = vec![0x14, bank << 4, addr, right, op, u8::try_from(body.len() + 1).unwrap()];
    v.extend_from_slice(body);
    v
}

fn req(entity: u8, slot: u8) -> Vec<u8> {
    vec![0x03, entity, slot]
}

fn cat(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.concat()
}

/// The one-entity section whose slot 1 is `code` (a RET is added) with these dialogs.
fn one(code: &[Vec<u8>], dialogs: Vec<Vec<u8>>) -> Vec<u8> {
    let mut code = cat(code);
    code.push(RET);
    SynthScript { dialogs, ..Default::default() }.entity("npc", vec![(1, code)]).build()
}

/// Every beat of a walk through `entity`'s `slot`, answering choices from `answers`.
fn run_slot(section: &[u8], entity: usize, slot: usize, answers: &[usize]) -> Vec<Beat> {
    run_with(section, entity, slot, answers, &StartOfGame)
}

fn run_with(section: &[u8], entity: usize, slot: usize, answers: &[usize], env: &dyn StoryVars) -> Vec<Beat> {
    let script = FieldScript::parse(section).unwrap();
    let mut walk = Walk::script(entity, slot, 7, Limits::default());
    let (mut out, mut answers, mut given) = (Vec::new(), answers.iter().copied(), None);
    for _ in 0..100 {
        let beat = walk.resume(&script, env, given.take());
        let asked = matches!(&beat, Beat::Message(m) if m.choice.is_some());
        let done = matches!(beat, Beat::End(_));
        out.push(beat);
        if done {
            break;
        }
        if asked {
            given = answers.next();
        }
    }
    out
}

fn run(section: &[u8], answers: &[usize]) -> Vec<Beat> {
    run_slot(section, 0, 1, answers)
}

/// The lines of the single page of a message beat.
fn lines(beat: &Beat) -> Vec<String> {
    match beat {
        Beat::Message(m) => m.page_lines().into_iter().next().unwrap_or_default(),
        Beat::End(e) => panic!("expected a message, got {e:?}"),
    }
}

fn ends(beats: &[Beat]) -> End {
    match beats.last() {
        Some(Beat::End(e)) => *e,
        other => panic!("the walk did not end: {other:?}"),
    }
}

fn messages(beats: &[Beat]) -> Vec<&Message> {
    beats.iter().filter_map(|b| if let Beat::Message(m) = b { Some(m) } else { None }).collect()
}

/// A hook that answers variable reads from a table (the rest 0), with Cloud alone in the party.
struct Vars(Vec<((u8, u8), u8)>);

impl StoryVars for Vars {
    fn byte(&self, bank: u8, address: u8) -> u8 {
        self.0.iter().find(|(k, _)| *k == (bank, address)).map_or(0, |(_, v)| *v)
    }
    fn party_member(&self, slot: u8) -> Option<u8> {
        (slot == 0).then_some(0)
    }
}

// ---- messages ----------------------------------------------------------------------------------

#[test]
fn a_talk_script_hands_out_its_dialog_and_then_ends() {
    let section = one(&[message(0, 1), message(0, 0)], vec![dialog("Second"), dialog("First")]);
    let beats = run(&section, &[]);
    assert_eq!(beats.len(), 3);
    assert_eq!(lines(&beats[0]), ["First"]);
    assert_eq!(lines(&beats[1]), ["Second"]);
    assert_eq!(beats[2], Beat::End(End::Finished));
    let Beat::Message(first) = &beats[0] else { panic!() };
    assert_eq!(first.origin, MessageOrigin::Canon { entity: 0, dialog: 1 });
    assert!(first.origin.is_canon() && first.choice.is_none());
}

#[test]
fn a_finished_walk_stays_finished() {
    let section = one(&[], vec![]);
    let script = FieldScript::parse(&section).unwrap();
    let mut walk = Walk::talk(0, 1, Limits::default());
    for _ in 0..3 {
        assert_eq!(walk.resume(&script, &StartOfGame, None), Beat::End(End::Finished));
    }
    // A slot without code, an entity that is not there: nothing to say, never a panic.
    assert_eq!(Walk::script(0, 31 + 1, 1, Limits::default()).resume(&script, &StartOfGame, None), Beat::End(End::Finished));
    assert_eq!(Walk::script(9, 1, 1, Limits::default()).resume(&script, &StartOfGame, None), Beat::End(End::Finished));
}

#[test]
fn lines_pages_colours_and_choice_markers_are_kept_in_order() {
    let mut text = Vec::new();
    text.extend(b"Biggs".iter().map(|b| b - 0x20));
    text.push(0xE7);
    text.extend([0xFE, 0xD7]);
    text.extend(b"Hi".iter().map(|b| b - 0x20));
    text.extend([0xE8, 0xE0]);
    text.extend(b"Bye".iter().map(|b| b - 0x20));
    text.push(0xFF);
    let section = one(&[message(0, 0)], vec![text]);
    let beats = run(&section, &[]);
    let Beat::Message(m) = &beats[0] else { panic!() };
    assert_eq!(
        m.pieces,
        [
            Piece::Text("Biggs".into()),
            Piece::NewLine,
            Piece::Colour(TextColour::Cyan),
            Piece::Text("Hi".into()),
            Piece::NewPage,
            Piece::Choice,
            Piece::Text("Bye".into()),
        ]
    );
    assert_eq!(m.page_lines(), vec![vec!["Biggs".to_string(), "Hi".to_string()], vec!["Bye".to_string()]]);
}

#[test]
fn a_dialog_that_is_not_in_the_table_shows_nothing_and_the_script_goes_on() {
    let section = one(&[message(0, 200), message(0, 0)], vec![dialog("Only")]);
    let beats = run(&section, &[]);
    assert_eq!(messages(&beats).len(), 1);
    assert_eq!(lines(&beats[0]), ["Only"]);
}

// ---- branches ----------------------------------------------------------------------------------

#[test]
fn a_branch_on_a_story_variable_follows_the_hook() {
    // if Var[1][5] == 1 { "late" }; "always"
    let section = one(&[if_ub(1, 5, 0, 1, &message(0, 0)), message(0, 1)], vec![dialog("late"), dialog("always")]);
    let start = run(&section, &[]);
    assert_eq!(messages(&start).len(), 1, "a new game: Var[1][5] is 0, the body is skipped");
    assert_eq!(lines(&start[0]), ["always"]);
    let later = run_with(&section, 0, 1, &[], &Vars(vec![((1, 5), 1)]));
    assert_eq!(messages(&later).len(), 2);
    assert_eq!(lines(&later[0]), ["late"]);
}

#[test]
fn all_eleven_operators_and_a_twelfth_that_is_false() {
    // Var[1][0] holds 0b0110 (6); each test guards one window; count the windows that open.
    let tests: [(u8, u8, bool); 12] = [
        (0, 6, true),
        (1, 6, false),
        (2, 5, true),
        (3, 7, true),
        (4, 6, true),
        (5, 5, false),
        (6, 4, true),  // 6 & 4 != 0
        (7, 6, false), // 6 ^ 6 == 0
        (8, 0, true),  // 6 | 0 != 0
        (9, 1, true),  // bit 1 of 6 is on
        (10, 1, false), // ... so "bit 1 is off" is false
        (11, 0, false),
    ];
    for (op, right, expect) in tests {
        let section = one(&[if_ub(1, 0, op, right, &message(0, 0))], vec![dialog("x")]);
        let beats = run_with(&section, 0, 1, &[], &Vars(vec![((1, 0), 6)]));
        assert_eq!(!messages(&beats).is_empty(), expect, "operator {op} against {right}");
    }
    // Bits past the width of the operand are off, and never a shift overflow.
    for right in [8u8, 31, 32, 200, 255] {
        for op in [9u8, 10] {
            let section = one(&[if_ub(1, 0, op, right, &message(0, 0))], vec![dialog("x")]);
            let beats = run_with(&section, 0, 1, &[], &Vars(vec![((1, 0), 0xFF)]));
            assert_eq!(!messages(&beats).is_empty(), op == 10, "bit {right} op {op}");
        }
    }
}

#[test]
fn variables_the_script_writes_are_what_it_reads_back() {
    let bit_op = |op: u8, addr: u8, bit: u8| vec![op, 15 << 4, addr, bit];
    // BITON bit 1 of Var[15][32]; a test of "bit 1 on" then opens the window.
    let section = one(&[bit_op(0x82, 32, 1), if_ub(15, 32, 9, 1, &message(0, 0))], vec![dialog("bit")]);
    assert_eq!(messages(&run(&section, &[])).len(), 1, "the walk reads its own write");
    // BITOFF puts it out again.
    let section = one(&[bit_op(0x82, 32, 1), bit_op(0x83, 32, 1), if_ub(15, 32, 9, 1, &message(0, 0))], vec![dialog("bit")]);
    assert_eq!(messages(&run(&section, &[])).len(), 0);
    // Another variable did not change.
    let section = one(&[bit_op(0x82, 32, 1), if_ub(15, 33, 9, 1, &message(0, 0))], vec![dialog("bit")]);
    assert_eq!(messages(&run(&section, &[])).len(), 0);
    // Bit 9 is bit 1 of the next byte (a row of bits), and BITXOR flips.
    let section = one(&[bit_op(0x82, 32, 9), if_ub(15, 33, 9, 1, &message(0, 0)), bit_op(0x84, 33, 1), if_ub(15, 33, 10, 1, &message(0, 0))], vec![dialog("bit")]);
    assert_eq!(messages(&run(&section, &[])).len(), 2);
}

#[test]
fn arithmetic_wraps_saturates_and_never_divides_by_zero() {
    // Var[2][0] = 250; PLUS 10 wraps to 4; the saturating PLUS! caps at 255; DIV and MOD by 0 do nothing.
    let plus = |op: u8, v: u8| vec![op, 2 << 4, 0, v];
    let section = one(
        &[setbyte(2, 0, 250), plus(0x85, 10), if_ub(2, 0, 0, 4, &message(0, 0)), setbyte(2, 0, 250), plus(0x76, 10), if_ub(2, 0, 0, 255, &message(0, 1)), plus(0x8B, 0), plus(0x8D, 0), if_ub(2, 0, 0, 255, &message(0, 2))],
        vec![dialog("wrap"), dialog("cap"), dialog("same")],
    );
    let beats = run(&section, &[]);
    assert_eq!(messages(&beats).len(), 3);
    // MINUS below zero wraps (255), MINUS! stops at zero.
    let minus = |op: u8, v: u8| vec![op, 2 << 4, 0, v];
    let section = one(
        &[setbyte(2, 0, 1), minus(0x87, 2), if_ub(2, 0, 0, 255, &message(0, 0)), setbyte(2, 0, 1), minus(0x78, 2), if_ub(2, 0, 0, 0, &message(0, 1))],
        vec![dialog("a"), dialog("b")],
    );
    assert_eq!(messages(&run(&section, &[])).len(), 2);
}

#[test]
fn random_follows_the_seed() {
    // RANDOM Var[2][1]; if it is >= 128 open a window.
    let section = one(&[vec![0x99, 2, 1], if_ub(2, 1, 4, 128, &message(0, 0))], vec![dialog("high")]);
    let script = FieldScript::parse(&section).unwrap();
    let outcome = |seed: u32| matches!(Walk::talk(0, seed, Limits::default()).resume(&script, &StartOfGame, None), Beat::Message(_));
    for seed in 0..20u32 {
        assert_eq!(outcome(seed), outcome(seed), "the same seed, the same walk");
    }
    assert!((0..40u32).any(outcome) && !(0..40u32).all(outcome), "both branches occur");
}

#[test]
fn key_tests_see_no_key_and_party_tests_ask_the_hook() {
    // IFKEY (keys 0x0001, skip body of 4 bytes) then IFPRTYQ Tifa then IFMEMBQ Cloud.
    let ifkey = |body: &[u8]| {
        let mut v = vec![0x30, 1, 0, u8::try_from(body.len() + 1).unwrap()];
        v.extend_from_slice(body);
        v
    };
    let ifprty = |op: u8, c: u8, body: &[u8]| {
        let mut v = vec![op, c, u8::try_from(body.len() + 1).unwrap()];
        v.extend_from_slice(body);
        v
    };
    let section = one(
        &[ifkey(&message(0, 0)), ifprty(0xCB, 2, &message(0, 1)), ifprty(0xCB, 0, &message(0, 2)), ifprty(0xCC, 1, &message(0, 3))],
        vec![dialog("key"), dialog("tifa"), dialog("cloud"), dialog("barret")],
    );
    let beats = run(&section, &[]);
    let said: Vec<_> = beats.iter().filter(|b| matches!(b, Beat::Message(_))).map(lines).collect();
    assert_eq!(said, [vec!["cloud".to_string()], vec!["barret".to_string()]], "Cloud and Barret are the start-of-game party");
}

// ---- asking ------------------------------------------------------------------------------------

/// "Question?" / `{CHOICE}Yes` / `{CHOICE}No`, the choices on lines 1 and 2.
fn question() -> Vec<u8> {
    let mut d: Vec<u8> = b"Question?".iter().map(|b| b - 0x20).collect();
    d.extend([0xE7, 0xE0]);
    d.extend(b"Yes".iter().map(|b| b - 0x20));
    d.extend([0xE7, 0xE0]);
    d.extend(b"No".iter().map(|b| b - 0x20));
    d.push(0xFF);
    d
}

fn asked_script() -> Vec<u8> {
    // ASK -> Var[5][19]; if it is 2, "No!" else "Yes!"; the second test needs its own skip of the first.
    one(
        &[ask(0, 1, 2, 5, 19), if_ub(5, 19, 0, 1, &message(0, 1)), if_ub(5, 19, 0, 2, &message(0, 2))],
        vec![question(), dialog("Yes!"), dialog("No!")],
    )
}

#[test]
fn an_ask_waits_for_an_answer_and_stores_the_absolute_line() {
    let section = asked_script();
    let beats = run(&section, &[2]);
    let Beat::Message(q) = &beats[0] else { panic!() };
    assert_eq!(q.choice, Some(Choice { first: 1, last: 2, cursor: 1 }));
    assert_eq!(lines(&beats[0]), ["Question?", "Yes", "No"]);
    assert_eq!(lines(&beats[1]), ["No!"], "the answer 2 is line 2, the second choice");
    assert_eq!(ends(&beats), End::Finished);
    assert_eq!(lines(&run(&section, &[1])[1]), ["Yes!"]);
    // No answer takes the cursor's line; an answer outside the choices is the nearest choice.
    assert_eq!(lines(&run(&section, &[])[1]), ["Yes!"]);
    assert_eq!(lines(&run(&section, &[99])[1]), ["No!"]);
    assert_eq!(lines(&run(&section, &[0])[1]), ["Yes!"]);
}

#[test]
fn the_cursor_starts_on_the_line_the_variable_holds() {
    let section = one(&[setbyte(5, 19, 2), ask(0, 1, 2, 5, 19)], vec![question()]);
    let beats = run(&section, &[]);
    let Beat::Message(q) = &beats[0] else { panic!() };
    assert_eq!(q.choice.map(|c| c.cursor), Some(2));
    // A value that is not a choice line starts on the first.
    let section = one(&[setbyte(5, 19, 7), ask(0, 1, 2, 5, 19)], vec![question()]);
    let Beat::Message(q) = &run(&section, &[])[0] else { panic!() };
    assert_eq!(q.choice.map(|c| c.cursor), Some(1));
}

#[test]
fn a_last_line_the_text_does_not_have_is_clamped_and_no_choice_line_makes_a_plain_window() {
    let section = one(&[ask(0, 1, 9, 5, 19)], vec![question()]);
    let Beat::Message(q) = &run(&section, &[])[0] else { panic!() };
    assert_eq!(q.choice, Some(Choice { first: 1, last: 2, cursor: 1 }));
    // The choices start past the text: a plain window, and the variable still holds an answer.
    let section = one(&[ask(0, 5, 6, 5, 19), if_ub(5, 19, 0, 5, &message(0, 1))], vec![question(), dialog("after")]);
    let beats = run(&section, &[]);
    let Beat::Message(q) = &beats[0] else { panic!() };
    assert_eq!(q.choice, None);
    assert_eq!(lines(&beats[1]), ["after"], "the script went on without waiting for an answer");
}

#[test]
fn a_page_break_restarts_the_line_count_of_the_choices() {
    // "Intro" NEWPAGE "Q?" / Yes / No: on the last page the choices are lines 1 and 2.
    let mut d: Vec<u8> = b"Intro".iter().map(|b| b - 0x20).collect();
    d.push(0xE8);
    d.extend(b"Q?".iter().map(|b| b - 0x20));
    d.extend([0xE7, 0xE0]);
    d.extend(b"Yes".iter().map(|b| b - 0x20));
    d.extend([0xE7, 0xE0]);
    d.extend(b"No".iter().map(|b| b - 0x20));
    d.push(0xFF);
    let section = one(&[ask(0, 1, 2, 5, 19)], vec![d]);
    let Beat::Message(q) = &run(&section, &[])[0] else { panic!() };
    assert_eq!(q.choice, Some(Choice { first: 1, last: 2, cursor: 1 }), "not clamped: the last page has three lines");
}

// ---- requests, ends, limits --------------------------------------------------------------------

#[test]
fn a_request_runs_the_requested_script_to_its_end_before_the_requester_goes_on() {
    let section = SynthScript { dialogs: vec![dialog("one"), dialog("two"), dialog("three")], ..Default::default() }
        .entity("a", vec![(1, cat(&[message(0, 0), req(1, 3), message(0, 2), vec![RET]]))])
        .entity("b", vec![(3, cat(&[message(0, 1), vec![RET]]))])
        .build();
    let beats = run(&section, &[]);
    assert_eq!(beats.iter().filter(|b| matches!(b, Beat::Message(_))).map(lines).collect::<Vec<_>>(), [["one"], ["two"], ["three"]]);
    let Beat::Message(second) = &beats[1] else { panic!() };
    assert_eq!(second.origin, MessageOrigin::Canon { entity: 1, dialog: 1 }, "the origin names the entity whose script spoke");
}

#[test]
fn an_unwaited_request_runs_until_it_loops_and_the_requester_goes_on() {
    // b's script 3 speaks and then idles in a JMPB 0 loop, as many of the engine's scripts do.
    let idle = cat(&[message(0, 1), vec![0x12, 0]]);
    let build = |op: u8| {
        SynthScript { dialogs: vec![dialog("one"), dialog("idler")], ..Default::default() }
            .entity("a", vec![(1, cat(&[vec![op, 1, 3], message(0, 0), vec![RET]]))])
            .entity("b", vec![(3, idle.clone())])
            .build()
    };
    for op in [0x01u8, 0x02] {
        let beats = run(&build(op), &[]);
        assert_eq!(beats.iter().filter(|b| matches!(b, Beat::Message(_))).map(lines).collect::<Vec<_>>(), [["idler"], ["one"]], "request opcode {op:#04x}");
        assert_eq!(ends(&beats), End::Finished);
    }
    // REQEW waits for the end for real: a script that never ends never lets its requester go on,
    // so the requester's own window ("one") is never shown and the conversation is over.
    let beats = run(&build(0x03), &[]);
    assert_eq!(messages(&beats).len(), 1);
    assert_eq!(lines(&beats[0]), ["idler"]);
    assert_eq!(ends(&beats), End::Finished);
}

#[test]
fn requests_nest_only_so_deep_and_a_script_that_requests_itself_ends() {
    let section = SynthScript { dialogs: vec![dialog("again")], ..Default::default() }
        .entity("a", vec![(1, cat(&[message(0, 0), req(0, 1), vec![RET]]))])
        .build();
    let limits = Limits { steps: 5_000, beats: 50, depth: 3 };
    let script = FieldScript::parse(&section).unwrap();
    let mut walk = Walk::talk(0, 1, limits);
    let mut windows = 0;
    loop {
        match walk.resume(&script, &StartOfGame, None) {
            Beat::Message(_) => windows += 1,
            Beat::End(e) => {
                assert_eq!(e, End::Finished);
                break;
            }
        }
    }
    assert_eq!(windows, limits.depth + 1, "the entry script and one more per allowed level of requests");
}

#[test]
fn an_idle_loop_ends_the_conversation_and_a_loop_that_counts_ends_in_the_budget() {
    // After its window the script idles in `JMPB 0` (a jump to itself): nothing can ever differ,
    // so the conversation is over at once, not after the budget.
    let section = one(&[message(0, 0), vec![0x12, 0]], vec![dialog("x")]);
    let script = FieldScript::parse(&section).unwrap();
    let mut walk = Walk::talk(0, 1, Limits { steps: 300, beats: 4, depth: 2 });
    assert!(matches!(walk.resume(&script, &StartOfGame, None), Beat::Message(_)));
    assert_eq!(walk.resume(&script, &StartOfGame, None), Beat::End(End::Finished));
    assert!(walk.steps() < 10, "{}", walk.steps());
    // The same with a wait in the loop and a test of a variable nobody changes.
    let wait = vec![0x24, 1, 0];
    let section = one(&[wait.clone(), if_ub(1, 0, 0, 1, &[0x00]), vec![0x12, u8::try_from(wait.len() + 7).unwrap()]], vec![]);
    let script = FieldScript::parse(&section).unwrap();
    let mut walk = Walk::talk(0, 1, Limits::default());
    assert_eq!(walk.resume(&script, &StartOfGame, None), Beat::End(End::Finished));
    assert!(walk.steps() < 10, "{}", walk.steps());
    // INC Var[2][0]; JMPB back to the INC: every lap differs, so only the budget stops it.
    let section = one(&[vec![0x95, 2, 0], vec![0x12, 3]], vec![]);
    let script = FieldScript::parse(&section).unwrap();
    let mut walk = Walk::talk(0, 1, Limits { steps: 300, beats: 4, depth: 2 });
    assert_eq!(walk.resume(&script, &StartOfGame, None), Beat::End(End::Budget));
    assert_eq!(walk.steps(), 300);
    // ... and a script that opens windows forever stops after the windows allowed.
    let section = one(&[message(0, 0), vec![0x12, 3]], vec![dialog("x")]);
    let script = FieldScript::parse(&section).unwrap();
    let mut walk = Walk::talk(0, 1, Limits { steps: 10_000, beats: 4, depth: 2 });
    let mut windows = 0;
    while let Beat::Message(_) = walk.resume(&script, &StartOfGame, None) {
        windows += 1;
    }
    assert_eq!((windows, walk.beats()), (4, 4));
}

#[test]
fn leaving_opcodes_end_the_conversation_and_say_how() {
    let mapjump = vec![0x60, 0x34, 0x12, 0, 0, 0, 0, 0, 0, 0];
    let battle = vec![0x70, 0, 5, 0];
    for (code, end) in [(mapjump, End::MapJump { field: 0x1234 }), (battle, End::Battle), (vec![0xFF], End::GameOver), (vec![0x20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], End::MiniGame)] {
        let section = one(&[message(0, 0), code, message(0, 0)], vec![dialog("before")]);
        let beats = run(&section, &[]);
        assert_eq!(beats.len(), 2, "{end:?}: the window before it, nothing after");
        assert_eq!(ends(&beats), end);
    }
}

#[test]
fn an_undecodable_script_ends_broken() {
    // 0x0C is not an opcode; a MESSAGE cut off by the end of the code is not decodable either.
    let section = one(&[message(0, 0), vec![0x0C]], vec![dialog("x")]);
    let beats = run(&section, &[]);
    assert_eq!(lines(&beats[0]), ["x"]);
    assert_eq!(ends(&beats), End::Broken);
}

// ---- who can be talked to ----------------------------------------------------------------------

/// A character entity: `CHAR 0; XYZI x y 0 0; [TALKR radius]; (if progress >= 7: TLKON 1; VISI 0); RET; JMPB 0`.
fn character(name: &str, x: i16, y: i16, radius: Option<u8>, hidden_late: bool, talk: Vec<u8>) -> SynthEntitySpec {
    let mut init = vec![0xA1, 0];
    init.extend([0xA5, 0, 0]);
    for v in [x, y, 0] {
        init.extend(v.to_le_bytes());
    }
    init.extend(0u16.to_le_bytes());
    if let Some(r) = radius {
        init.extend([0xC5, 0, r]);
    }
    if hidden_late {
        // IFSW Var[2][0] >= 7: bank byte 0x20 (bank 2 on the left, a literal on the right), the
        // left address as a word, the right literal as a word, operator 4 (>=), the jump.
        let body = vec![0x7E, 1, 0xA4, 0];
        init.extend([0x16, 0x20, 0, 0, 7, 0, 4, u8::try_from(body.len() + 1).unwrap()]);
        init.extend(body);
    }
    init.push(RET);
    init.extend([0x12, 0]);
    SynthEntitySpec { name: name.to_string(), scripts: vec![(0, init), (1, talk)] }
}

struct SynthEntitySpec {
    name: String,
    scripts: Vec<(usize, Vec<u8>)>,
}

fn field(chars: Vec<SynthEntitySpec>, dialogs: Vec<Vec<u8>>, scale: u16) -> Vec<u8> {
    let mut s = SynthScript { dialogs, scale, model_count: 1, ..Default::default() };
    for c in chars {
        s = s.entity(&c.name, c.scripts);
    }
    s.build()
}

#[test]
fn a_character_that_the_story_has_hidden_is_not_talkable_and_the_hook_decides() {
    let talk = cat(&[message(0, 0), vec![RET]]);
    let section = field(vec![character("gu0", 100, 200, Some(70), true, talk)], vec![dialog("Hello")], 512);
    let script = FieldScript::parse(&section).unwrap();
    assert_eq!(availability(&script, 0, &StartOfGame), Availability { talkable: true, visible: true });
    let late = Vars(vec![((2, 0), 7)]);
    assert_eq!(availability(&script, 0, &late), Availability { talkable: false, visible: false });
    // A talk session follows.
    let mut now = TalkSession::talk(section.clone(), 0, Box::new(StartOfGame), 1).unwrap();
    assert_eq!(lines(&now.next(None)), ["Hello"]);
    assert_eq!(now.next(None), Beat::End(End::Finished));
    assert_eq!(TalkSession::talk(section.clone(), 0, Box::new(late), 1).unwrap_err(), TalkError::NotTalkable);
    assert_eq!(TalkSession::talk(section.clone(), 5, Box::new(StartOfGame), 1).unwrap_err(), TalkError::NoSuchEntity);
    assert_eq!(TalkSession::talk(vec![1, 2, 3], 0, Box::new(StartOfGame), 1).unwrap_err(), TalkError::BadSection);
    // The cutscene lines of any slot are reachable whatever the story says.
    let mut any = TalkSession::script(section, 0, 1, Box::new(Vars(vec![((2, 0), 7)])), 1).unwrap();
    assert_eq!(lines(&any.next(None)), ["Hello"]);
}

#[test]
fn the_prelude_cuts_the_idle_loop_and_skips_a_script_requested_twice() {
    // Init: REQEW self 5 twice (script 5 hides the character); the main part loops.
    let hide = cat(&[vec![0xA4, 0], vec![RET]]);
    let mut init = vec![0xA1, 0, 0x03, 0, 5, 0x03, 0, 5, RET];
    init.extend([0x12, 0]);
    let section = SynthScript { dialogs: vec![], model_count: 1, ..Default::default() }.entity("x", vec![(0, init), (5, hide)]).build();
    let script = FieldScript::parse(&section).unwrap();
    assert_eq!(availability(&script, 0, &StartOfGame), Availability { talkable: true, visible: false });
}

#[test]
fn characters_are_found_by_name_and_place_and_the_radius_scales_with_the_field() {
    let silent = vec![RET];
    let section = field(
        vec![
            character("gu0", 100, 200, Some(70), false, silent.clone()),
            character("gu0", 500, -300, None, false, silent.clone()),
            character("", 10, 10, Some(0), false, silent.clone()),
            character("solo", 7, 8, Some(255), false, silent),
        ],
        vec![],
        1024,
    );
    let script = FieldScript::parse(&section).unwrap();
    let chars = characters(&script);
    assert_eq!(chars.iter().map(|c| c.entity).collect::<Vec<_>>(), [0, 1, 2, 3]);
    assert_eq!(chars[0].places, [(100, 200)]);
    // TALKR 70 and the default 80 at scale 1024 (twice 512), TALKR 0 stays 0, and 255 doubles.
    assert_eq!(chars.iter().map(|c| c.talk_radius).collect::<Vec<_>>(), [140.0, 160.0, 0.0, 510.0]);
    // Two characters called gu0: the place decides.
    assert_eq!(find_character(&chars, "gu0", 100.0, 200.0).map(|c| c.entity), Some(0));
    assert_eq!(find_character(&chars, "gu0", 500.4, -299.6).map(|c| c.entity), Some(1));
    // An empty name is matched by its place too; a name that is not unique and a place that matches
    // nothing find nobody; a moved character is found by its unique name.
    assert_eq!(find_character(&chars, "", 10.0, 10.0).map(|c| c.entity), Some(2));
    assert_eq!(find_character(&chars, "gu0", 0.0, 0.0), None);
    assert_eq!(find_character(&chars, "", 99.0, 99.0), None);
    assert_eq!(find_character(&chars, "solo", 900.0, 900.0).map(|c| c.entity), Some(3));
    assert_eq!(find_character(&chars, "nobody", 7.0, 8.0), None);
    assert_eq!(find_character(&chars, "gu0", f32::NAN, 0.0), None);
}

// ---- message sources ---------------------------------------------------------------------------

/// A source that is not the game's: one added line, then nothing.
struct Added(Option<Message>);

impl MessageSource for Added {
    fn next(&mut self, _answer: Option<usize>) -> Beat {
        self.0.take().map_or(Beat::End(End::Finished), Beat::Message)
    }
}

#[test]
fn an_added_line_is_a_message_like_any_other_but_never_canon() {
    let added = Message::new(MessageOrigin::Added { layer: "player-lines".into() }, vec![Piece::Text("Hello, traveller.".into())]);
    let mut sources: Vec<Box<dyn MessageSource>> = vec![
        Box::new(Added(Some(added.clone()))),
        Box::new(TalkSession::talk(one(&[message(0, 0)], vec![dialog("Canon")]), 0, Box::new(StartOfGame), 1).unwrap()),
    ];
    let first = sources[0].next(None);
    let second = sources[1].next(None);
    let (Beat::Message(a), Beat::Message(c)) = (first, second) else { panic!() };
    assert!(!a.origin.is_canon() && c.origin.is_canon());
    assert_eq!(a, added);
    assert_eq!(a.page_lines(), vec![vec!["Hello, traveller.".to_string()]]);
}

// ---- hostile input -----------------------------------------------------------------------------

/// A small deterministic generator for the fuzz tests.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(n.max(1)).unwrap()).unwrap()
    }
    fn byte(&mut self) -> u8 {
        u8::try_from(self.next() >> 56).unwrap()
    }
    fn bytes(&mut self, max: usize) -> Vec<u8> {
        let n = self.below(max + 1);
        (0..n).map(|_| self.byte()).collect()
    }
}

/// Run `section` as hard as a client would: every entity, slots 0 to 6, answering at random, with
/// small limits; the counters must respect them and nothing may panic.
fn hammer(section: &[u8], rng: &mut Rng) {
    let Ok(script) = FieldScript::parse(section) else { return };
    let limits = Limits { steps: 300, beats: 6, depth: 3 };
    for entity in 0..script.entity_count().min(4) {
        let _ = availability(&script, entity, &StartOfGame);
        for slot in 0..7 {
            let mut walk = Walk::script(entity, slot, 1, limits);
            let mut given = None;
            for _ in 0..40 {
                let beat = walk.resume(&script, &StartOfGame, given.take());
                match beat {
                    Beat::End(_) => break,
                    Beat::Message(m) => {
                        if let Some(c) = m.choice {
                            assert!(c.first <= c.cursor && c.cursor <= c.last, "{c:?}");
                            given = Some(rng.below(300));
                        }
                    }
                }
            }
            assert!(walk.steps() <= limits.steps && walk.beats() <= limits.beats);
        }
    }
}

#[test]
fn random_scripts_never_panic_and_never_run_past_their_limits() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for _ in 0..4000 {
        let mut s = SynthScript { model_count: 1, ..Default::default() };
        for _ in 0..(1 + rng.below(3)) {
            let name = format!("e{}", rng.below(9));
            let mut scripts = Vec::new();
            for slot in 0..7 {
                if rng.below(2) == 0 {
                    scripts.push((slot, rng.bytes(48)));
                }
            }
            s = s.entity(&name, scripts);
        }
        for _ in 0..rng.below(4) {
            let mut d = rng.bytes(40);
            d.push(0xFF);
            s.dialogs.push(d);
        }
        hammer(&s.build(), &mut rng);
    }
}

#[test]
fn mutated_real_shaped_scripts_never_panic() {
    // The scripts of this file's own tests, with bytes flipped, cut and extended.
    let seeds = [asked_script(), one(&[message(0, 0), req(0, 1), if_ub(1, 5, 0, 1, &message(0, 0))], vec![dialog("loop")])];
    let mut rng = Rng(0xDEAD_BEEF_1234_5678);
    for seed in &seeds {
        for _ in 0..3000 {
            let mut bytes = seed.clone();
            for _ in 0..(1 + rng.below(6)) {
                let at = rng.below(bytes.len());
                match rng.below(3) {
                    0 => {
                        let b = rng.byte();
                        if let Some(slot) = bytes.get_mut(at) {
                            *slot = b;
                        }
                    }
                    1 => bytes.truncate(at.max(1)),
                    _ => bytes.push(rng.byte()),
                }
            }
            hammer(&bytes, &mut rng);
        }
    }
}

#[test]
fn every_opcode_with_every_filler_is_survived_by_the_walker() {
    for opcode in 0..=255u8 {
        for filler in [0u8, 1, 0x7F, 0x80, 0xFF] {
            let mut code = vec![opcode];
            code.extend_from_slice(&[filler; 16]);
            let section = SynthScript { dialogs: vec![dialog("x")], model_count: 1, ..Default::default() }.entity("e", vec![(1, code.clone()), (0, code)]).build();
            hammer(&section, &mut Rng(u64::from(opcode) * 31 + u64::from(filler) + 1));
        }
    }
}
