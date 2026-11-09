//! Tests of the field-sound runner and its plans on synthetic scripts (never real game data).

use super::run::{instant, Banked};
use super::*;
use crate::field::Vertex;
use crate::script::synth::SynthScript;
use crate::script::Instruction;
use crate::story::REACTOR_LIFT;

// ---- a tiny assembler ---------------------------------------------------------------------------

fn cat(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.concat()
}
fn ret() -> Vec<u8> {
    vec![0x00]
}
/// `SOUND id pan`.
fn sound(id: u16, pan: u8) -> Vec<u8> {
    let i = id.to_le_bytes();
    vec![0xF1, 0x00, i[0], i[1], pan]
}
/// `AKAO op pan id` (the operands the owned scripts use for the sound plays, `p3` to `p5` zero).
fn akao(op: u8, pan: u8, id: u16) -> Vec<u8> {
    let i = id.to_le_bytes();
    vec![0xF2, 0, 0, 0, op, pan, i[0], i[1], 0, 0, 0, 0, 0, 0]
}
/// A play of sound `id` on effect channel `channel` (1 to 4), centred.
fn play(channel: u8, id: u16) -> Vec<u8> {
    akao(39 + channel, 64, id)
}
fn stop(channel: u8) -> Vec<u8> {
    play(channel, 0)
}
fn wait(n: u16) -> Vec<u8> {
    let b = n.to_le_bytes();
    vec![0x24, b[0], b[1]]
}
fn req(entity: u8, slot: u8) -> Vec<u8> {
    vec![0x01, entity, slot]
}
fn reqew(entity: u8, slot: u8) -> Vec<u8> {
    vec![0x03, entity, slot]
}
/// `MOVE x y` (a walk: how long it takes is not in the script).
fn move_to(x: i16, y: i16) -> Vec<u8> {
    let mut v = vec![0xA8, 0];
    v.extend(x.to_le_bytes());
    v.extend(y.to_le_bytes());
    v
}
/// `JUMP x y triangle 17`.
fn jump_to(x: i16, y: i16, triangle: u16) -> Vec<u8> {
    let mut v = vec![0xC0, 0, 0];
    v.extend(x.to_le_bytes());
    v.extend(y.to_le_bytes());
    v.extend(triangle.to_le_bytes());
    v.extend(17i16.to_le_bytes());
    v
}
/// `LADER x y z triangle`.
fn lader(x: i16, y: i16, z: i16, triangle: u16) -> Vec<u8> {
    let mut v = vec![0xC2, 0, 0];
    v.extend(x.to_le_bytes());
    v.extend(y.to_le_bytes());
    v.extend(z.to_le_bytes());
    v.extend(triangle.to_le_bytes());
    v.extend([1, 5, 64, 1]);
    v
}
fn scrlw() -> Vec<u8> {
    vec![0x67]
}
/// `CANM!1 animation 4, frames 0 to 39, speed 1`: an animation that is waited for.
fn canm1() -> Vec<u8> {
    vec![0xB1, 4, 0, 39, 1]
}
fn dfanm() -> Vec<u8> {
    vec![0xA2, 4, 1]
}
/// `SETWORD Var[bank][addr] = value`.
fn setword(bank: u8, addr: u8, value: i16) -> Vec<u8> {
    let v = value.to_le_bytes();
    vec![0x81, bank << 4, addr, v[0], v[1]]
}
/// `BITON` / `BITOFF` of `Var[bank][addr]` bit `bit`.
fn biton(bank: u8, addr: u8, bit: u8) -> Vec<u8> {
    vec![0x82, bank << 4, addr, bit]
}
fn bitoff(bank: u8, addr: u8, bit: u8) -> Vec<u8> {
    vec![0x83, bank << 4, addr, bit]
}
/// `IFSW Var[bank][addr] <op> literal` guarding `body` (the test falls through into the body when it holds).
fn when_lit(bank: u8, addr: u8, op: u8, literal: i16, body: &[u8]) -> Vec<u8> {
    let l = literal.to_le_bytes();
    let mut v = vec![0x16, bank << 4, addr, 0, l[0], l[1], op, u8::try_from(body.len() + 1).unwrap()];
    v.extend_from_slice(body);
    v
}
/// `IFUB Var[bank][addr] <op> literal` guarding `body`.
fn when_byte(bank: u8, addr: u8, op: u8, literal: u8, body: &[u8]) -> Vec<u8> {
    let mut v = vec![0x14, bank << 4, addr, literal, op, u8::try_from(body.len() + 1).unwrap()];
    v.extend_from_slice(body);
    v
}
/// `if Var[2][0] <op> literal { then } else { other }` (the shape the owned scripts use: a forward jump over the else).
fn if_moment(op: u8, literal: i16, then: &[u8], other: &[u8]) -> Vec<u8> {
    let jump_over_else = vec![0x10, u8::try_from(other.len() + 1).unwrap()];
    cat(&[when_lit(2, 0, op, literal, &cat(&[then.to_vec(), jump_over_else])), other.to_vec()])
}
/// `IFKEYON keys` guarding `body`.
fn when_key(keys: u16, body: &[u8]) -> Vec<u8> {
    let mut v = vec![0x31];
    v.extend(keys.to_le_bytes());
    v.push(u8::try_from(body.len() + 1).unwrap());
    v.extend_from_slice(body);
    v
}
/// A backward jump over the `body` in front of it (`JMPB` counts from its own opcode).
fn forever(body: &[u8]) -> Vec<u8> {
    let mut v = body.to_vec();
    v.extend([0x12, u8::try_from(body.len()).unwrap()]);
    v
}
fn mapjump(field: u16, x: i16, y: i16, triangle: i16) -> Vec<u8> {
    let mut v = vec![0x60];
    v.extend(field.to_le_bytes());
    v.extend(x.to_le_bytes());
    v.extend(y.to_le_bytes());
    v.extend(triangle.to_le_bytes());
    v.push(0);
    v
}
/// `LINE` between two points at height `z`.
fn line(a: (i16, i16), b: (i16, i16), z: i16) -> Vec<u8> {
    let mut v = vec![0xD0];
    for c in [a.0, a.1, z, b.0, b.1, z] {
        v.extend_from_slice(&c.to_le_bytes());
    }
    v
}

/// An entity's scripts, as `(slot, code)`.
type Scripts = Vec<(usize, Vec<u8>)>;

/// A section of entities, each `(name, scripts)`.
fn section(entities: Vec<(&str, Scripts)>) -> Vec<u8> {
    let mut script = SynthScript { model_count: 1, scale: 512, ..Default::default() };
    for (name, scripts) in entities {
        script = script.entity(name, scripts);
    }
    script.build()
}

/// A director: its init part, then (after a `RET`) its main part.
fn director(init: &[u8], main: &[u8]) -> (&'static str, Scripts) {
    ("dir", vec![(0, cat(&[init.to_vec(), ret(), main.to_vec(), ret()]))])
}

fn parsed(bytes: &[u8]) -> FieldScript<'_> {
    FieldScript::parse(bytes).unwrap()
}

fn story(moment: u16) -> StoryState {
    StoryState::at(moment)
}

/// The plays of an event list as `(frames, channel, id)`.
fn plays(events: &[Timed]) -> Vec<(u32, u8, u16)> {
    events.iter().map(|e| (e.at, e.call.channel, e.call.id)).collect()
}

/// Run script `slot` of entity `entity` whole, for `story` and `keys`.
fn run_slot(bytes: &[u8], entity: usize, slot: u8, story: &StoryState, keys: u32) -> (Vec<Timed>, Stop, Machine<'static>) {
    // (the section is leaked for the test's sake: the machine borrows it for as long as it lives)
    let leaked: &'static [u8] = Box::leak(bytes.to_vec().into_boxed_slice());
    let script: &'static FieldScript<'static> = Box::leak(Box::new(FieldScript::parse(leaked).unwrap()));
    let story: &'static StoryState = Box::leak(Box::new(story.clone()));
    let mut machine = Machine::new(script, Env { story, keys });
    let run = machine.run(ScriptRef { entity, slot }, Part::Whole, None, 0);
    let stop = machine.runs[run].stop;
    (machine.events.clone(), stop, machine)
}

// ---- the opcodes ---------------------------------------------------------------------------------

fn instruction(code: &[u8]) -> Instruction {
    crate::script::decode_at(code, 0, 0).unwrap()
}

#[test]
fn sound_is_channel_one_and_akao_40_to_43_are_channels_one_to_four() {
    let s = Call::of(&instruction(&sound(62, 64))).unwrap().unwrap();
    assert_eq!((s.channel, s.id, s.pan, s.source), (1, 62, 64, Source::Sound));
    for (op, channel) in [(40u8, 1u8), (41, 2), (42, 3), (43, 4)] {
        let c = Call::of(&instruction(&akao(op, 63, 38))).unwrap().unwrap();
        assert_eq!((c.channel, c.id, c.pan, c.source), (channel, 38, 63, Source::Akao), "op {op}");
    }
    // sound id 0 is the stop of the channel
    assert_eq!(Call::of(&instruction(&stop(3))).unwrap().map(|c| (c.channel, c.id)), Some((3, 0)));
    // an AKAO operation that is not a sound play (160: a channel's volume; 32: a music change) is not a call
    for op in [32u8, 39, 44, 160, 175, 200] {
        assert_eq!(Call::of(&instruction(&akao(op, 64, 5))), Ok(None), "op {op}");
    }
    // any other instruction is not a call
    assert_eq!(Call::of(&instruction(&wait(3))), Ok(None));
}

#[test]
fn an_akao2_play_has_the_same_operands_in_wider_fields() {
    // AKAO2: [DA][3 bank bytes][op][p1 u16][p2 u16][p3 u16][p4 u16][p5 u16]
    let mut code = vec![0xDA, 0, 0, 0, 41];
    for v in [63u16, 427, 0, 0, 0] {
        code.extend(v.to_le_bytes());
    }
    let c = Call::of(&instruction(&code)).unwrap().unwrap();
    assert_eq!((c.channel, c.id, c.pan, c.source), (2, 427, 63, Source::Akao2));
}

#[test]
fn a_sound_whose_operands_are_variables_is_banked_not_guessed() {
    // SOUND with the id in a variable: the bank nibble of the id is not 0
    let banked = vec![0xF1, 0x10, 7, 0, 64];
    assert_eq!(Call::of(&instruction(&banked)), Err(Banked));
    let bytes = section(vec![("a", vec![(0, cat(&[banked, play(2, 9), ret()]))])]);
    let start = start_up(&parsed(&bytes), &story(1));
    assert_eq!(plays(&start.events), [(0, 2, 9)], "the banked one is skipped, the next one is played");
    assert_eq!(start.banked, 1);
}

// ---- the runner ----------------------------------------------------------------------------------

#[test]
fn plays_come_with_their_times_and_the_run_stops_where_it_cannot_say_how_long_something_takes() {
    // the jump of nmkin_3, as its data have it: an animation start, half a second, the take-off sound, the jump
    let jump = cat(&[dfanm(), wait(15), sound(62, 64), jump_to(60, 1919, 42), wait(20), sound(21, 64), ret()]);
    let bytes = section(vec![("hero", vec![(3, jump)])]);
    let (events, stop, _) = run_slot(&bytes, 0, 3, &story(13), 0);
    assert_eq!(plays(&events), [(15, 1, 62)], "the take-off at half a second (15 frames at 30 per second), nothing after the jump");
    assert!(matches!(stop, Stop::Unknown { opcode: 0xC0, .. }), "{stop:?}");
    assert_eq!(seconds(15), 0.5);
}

#[test]
fn a_script_that_returns_has_a_known_length_and_every_play_is_in_the_list() {
    let bytes = section(vec![("a", vec![(3, cat(&[sound(1, 64), wait(30), play(3, 2), wait(30), stop(3), ret()]))])]);
    let (events, stop, machine) = run_slot(&bytes, 0, 3, &story(1), 0);
    assert_eq!(plays(&events), [(0, 1, 1), (30, 3, 2), (60, 3, 0)]);
    assert_eq!(stop, Stop::Returned);
    assert_eq!(machine.runs[0].frames, Some(60));
    assert_eq!(machine.runs[0].open, 0, "a clean end leaves nothing open");
}

#[test]
fn a_backward_jump_stops_the_run_and_leaves_open_the_channels_its_body_plays_on() {
    // main: a loop that waits and plays channel 2 each time round; channel 3 was set before it
    let main = cat(&[play(3, 38), forever(&cat(&[wait(60), play(2, 9)]))]);
    let bytes = section(vec![("a", vec![(3, main)])]);
    let (events, stop, machine) = run_slot(&bytes, 0, 3, &story(1), 0);
    assert_eq!(plays(&events), [(0, 3, 38), (60, 2, 9)], "the first pass is run, the loop is not run round again");
    assert!(matches!(stop, Stop::IdleLoop { .. }), "{stop:?}");
    assert_eq!(machine.runs[0].open, 0b0010, "channel 2 plays inside the loop, channel 3 does not");
}

#[test]
fn an_if_on_the_moment_picks_the_branch_for_the_story_the_player_has() {
    let script = if_moment(0, 27, &play(2, 60), &play(3, 38));
    let bytes = section(vec![("a", vec![(3, cat(&[script, ret()]))])]);
    for (moment, expect) in [(15, (3, 38)), (27, (2, 60)), (26, (3, 38)), (28, (3, 38))] {
        let (events, stop, _) = run_slot(&bytes, 0, 3, &story(moment), 0);
        assert_eq!(plays(&events), [(0, expect.0, expect.1)], "moment {moment}");
        assert_eq!(stop, Stop::Returned);
    }
    // the other operators, on both sides of the literal
    for (op, moment, plays_it) in [(1u8, 27u16, false), (1, 5, true), (2, 28, true), (2, 27, false), (3, 26, true), (3, 27, false), (4, 27, true), (4, 26, false), (5, 27, true), (5, 28, false)] {
        let bytes = section(vec![("a", vec![(3, cat(&[when_lit(2, 0, op, 27, &play(1, 7)), ret()]))])]);
        let (events, _, _) = run_slot(&bytes, 0, 3, &story(moment), 0);
        assert_eq!(!events.is_empty(), plays_it, "op {op} moment {moment}");
    }
}

#[test]
fn a_test_of_a_tracked_bit_reads_that_bit_and_one_of_an_untracked_variable_stops_the_run_and_is_counted() {
    // IFUB Var[1][225] bit-off 0 (op 10): true while the lift is up
    let lift_up = cat(&[when_byte(REACTOR_LIFT.bank, REACTOR_LIFT.address, 10, 0, &play(1, 42)), ret()]);
    let bytes = section(vec![("a", vec![(3, lift_up)])]);
    let mut down = story(12);
    assert!(down.set_bit(REACTOR_LIFT.bank, REACTOR_LIFT.address, 0, true));
    assert_eq!(plays(&run_slot(&bytes, 0, 3, &story(12), 0).0), [(0, 1, 42)], "the bit is clear: the test holds");
    assert!(run_slot(&bytes, 0, 3, &down, 0).0.is_empty(), "the bit is set: it does not");
    // Var[5][1] is nobody's story: the run stops there, the sound after it is not played, and the test is counted
    let stranger = cat(&[play(2, 5), when_byte(5, 1, 5, 11, &play(3, 6)), play(4, 7), ret()]);
    let bytes = section(vec![("a", vec![(3, stranger)])]);
    let (events, stop, machine) = run_slot(&bytes, 0, 3, &story(12), 0);
    assert_eq!(plays(&events), [(0, 2, 5)]);
    assert!(matches!(stop, Stop::Unjudged { .. }), "{stop:?}");
    assert_eq!(machine.unjudged.len(), 1);
    assert_eq!(machine.unjudged[0].test, "Var[5][1] <= 11", "the test as the script states it");
    assert_eq!(machine.runs[0].open, 0b1100, "channels 3 and 4 play in the part that was not reached");
}

#[test]
fn a_write_to_the_moment_stops_the_run_because_the_story_is_no_longer_the_scripts() {
    let bytes = section(vec![("a", vec![(3, cat(&[play(1, 1), setword(2, 0, 12), when_lit(2, 0, 3, 13, &play(2, 2)), ret()]))])]);
    let (events, stop, _) = run_slot(&bytes, 0, 3, &story(11), 0);
    assert_eq!(plays(&events), [(0, 1, 1)]);
    assert!(matches!(stop, Stop::Unknown { opcode: 0x81, .. }), "{stop:?}");
    // a variable the story does not carry may be written freely (the run does not track it)
    let bytes = section(vec![("a", vec![(3, cat(&[setword(6, 2, 29), play(1, 1), ret()]))])]);
    assert_eq!(plays(&run_slot(&bytes, 0, 3, &story(11), 0).0), [(0, 1, 1)]);
}

#[test]
fn keys_are_whatever_the_run_was_told_is_held() {
    let script = cat(&[when_key(0x220, &play(1, 21)), ret()]);
    let bytes = section(vec![("a", vec![(3, script)])]);
    assert!(run_slot(&bytes, 0, 3, &story(1), 0).0.is_empty(), "nothing held: the ladder waits");
    assert_eq!(plays(&run_slot(&bytes, 0, 3, &story(1), crate::script::dialogue::KEY_OK).0), [(0, 1, 21)]);
}

#[test]
fn req_starts_a_script_at_once_and_reqew_waits_for_it() {
    let callee = cat(&[wait(10), sound(5, 64), wait(20), ret()]);
    // REQ: the caller goes on at the same time
    let bytes = section(vec![("a", vec![(3, cat(&[req(1, 3), sound(1, 64), ret()]))]), ("b", vec![(3, callee.clone())])]);
    let (events, stop, _) = run_slot(&bytes, 0, 3, &story(1), 0);
    assert_eq!(plays(&events), [(10, 1, 5), (0, 1, 1)], "the callee's sound is at 10 frames, the caller's at once (the list is in the order met)");
    assert_eq!(stop, Stop::Returned);
    // REQEW: the caller waits the callee's 30 frames
    let bytes = section(vec![("a", vec![(3, cat(&[reqew(1, 3), sound(1, 64), ret()]))]), ("b", vec![(3, callee)])]);
    let (events, _, machine) = run_slot(&bytes, 0, 3, &story(1), 0);
    assert_eq!(plays(&events), [(10, 1, 5), (30, 1, 1)]);
    assert_eq!(machine.runs.len(), 2);
    assert_eq!(machine.runs[1].parent, Some(0));
}

#[test]
fn a_callee_that_never_ends_stops_the_caller_that_waits_for_it_and_not_the_one_that_does_not() {
    let callee = forever(&cat(&[wait(30), play(2, 9)]));
    let bytes = section(vec![("a", vec![(3, cat(&[reqew(1, 3), sound(1, 64), ret()]))]), ("b", vec![(3, callee.clone())])]);
    let (events, stop, machine) = run_slot(&bytes, 0, 3, &story(1), 0);
    assert_eq!(plays(&events), [(30, 2, 9)], "the callee's first pass is its own; the caller's sound after the wait is not played");
    assert!(matches!(stop, Stop::Unknown { opcode: 0x03, .. }), "{stop:?}");
    assert!(matches!(machine.runs[1].stop, Stop::IdleLoop { .. }));
    let bytes = section(vec![("a", vec![(3, cat(&[req(1, 3), sound(1, 64), ret()]))]), ("b", vec![(3, callee)])]);
    let (events, stop, _) = run_slot(&bytes, 0, 3, &story(1), 0);
    assert_eq!(plays(&events), [(30, 2, 9), (0, 1, 1)]);
    assert_eq!(stop, Stop::Returned);
}

#[test]
fn a_script_that_calls_itself_or_its_caller_is_not_followed_round_again() {
    let bytes = section(vec![("a", vec![(3, cat(&[sound(1, 64), req(1, 3), ret()]))]), ("b", vec![(3, cat(&[sound(2, 64), req(0, 3), ret()]))])]);
    let (events, stop, machine) = run_slot(&bytes, 0, 3, &story(1), 0);
    assert_eq!(plays(&events), [(0, 1, 1), (0, 1, 2)]);
    assert!(matches!(machine.runs[1].stop, Stop::Unknown { opcode: 0x01, .. }), "the call back into a script already running stops: {:?}", machine.runs[1].stop);
    assert_eq!(stop, Stop::Returned, "the first caller does not wait for it and goes on");
    // a call to an entity that does not exist
    let bytes = section(vec![("a", vec![(3, cat(&[req(9, 3), ret()]))])]);
    assert!(matches!(run_slot(&bytes, 0, 3, &story(1), 0).1, Stop::Unknown { .. }));
}

#[test]
fn a_callee_that_leaves_the_field_takes_the_caller_with_it() {
    let bytes = section(vec![("a", vec![(3, cat(&[reqew(1, 3), sound(1, 64), ret()]))]), ("b", vec![(3, cat(&[sound(55, 64), mapjump(5, 0, 0, 0), ret()]))])]);
    let (events, stop, _) = run_slot(&bytes, 0, 3, &story(1), 0);
    assert_eq!(plays(&events), [(0, 1, 55)], "nothing after the jump");
    assert!(matches!(stop, Stop::LeavesField { .. }), "{stop:?}");
}

#[test]
fn a_script_with_no_end_marker_and_a_jump_past_its_end_return_instead_of_running_on() {
    // the data's quirk: a script falls off its end into the RET that follows it
    let bytes = section(vec![("a", vec![(3, play(1, 3))])]);
    let (events, stop, _) = run_slot(&bytes, 0, 3, &story(1), 0);
    assert_eq!(plays(&events), [(0, 1, 3)]);
    assert_eq!(stop, Stop::Returned);
}

#[test]
fn the_list_of_instant_opcodes_does_not_hold_what_takes_time() {
    // MOVE, CMOVE, FMOVE, MOVA, TURA, ANIMW, ANIM!1, CANM!1, JUMP, LADER, OFSTW, SCRLW, FADEW, MESSAGE, ASK, MENU, TURNW, the
    // party requests
    for opcode in [0xA8u8, 0xA9, 0xAD, 0xAA, 0xAB, 0xAC, 0xAF, 0xB1, 0xBA, 0xBC, 0xC0, 0xC2, 0xC4, 0x67, 0x6C, 0x40, 0x48, 0x49, 0xDE, 0x04, 0x05, 0x06] {
        assert!(!instant(opcode), "{opcode:#04x} takes time");
    }
    // and does hold what does not
    for opcode in [0x80u8, 0x81, 0x82, 0x83, 0x99, 0xA2, 0xA5, 0xB3, 0xF0, 0xF1, 0xF2, 0xDA, 0x64, 0x66, 0xE0, 0x33, 0x4A] {
        assert!(instant(opcode), "{opcode:#04x} is instant");
    }
}

// ---- what a field plays by itself ----------------------------------------------------------------

/// The ids a test's clips loop on: the reactor hum (38), the alarm (60) and a long drone (95).
fn loops(id: u16) -> bool {
    matches!(id, 38 | 60 | 95 | 242)
}

#[test]
fn the_reactor_hum_gives_way_to_the_alarm_when_the_moment_is_27() {
    // nmkin_2's director, in shape: after the boss (moment 27) the alarm on channel 2, before it the hum on channel 3
    let init = if_moment(0, 27, &play(2, 60), &play(3, 38));
    let bytes = section(vec![director(&init, &[])]);
    let script = parsed(&bytes);
    let before = start_up(&script, &story(15)).ambient(loops);
    assert_eq!(before.iter().map(|a| (a.channel, a.id, a.at)).collect::<Vec<_>>(), [(3, 38, 0)]);
    let after = start_up(&script, &story(27)).ambient(loops);
    assert_eq!(after.iter().map(|a| (a.channel, a.id, a.at)).collect::<Vec<_>>(), [(2, 60, 0)]);
    assert_eq!(after[0].pan, 64);
    // a story that moves a long way past it has the hum again (the field's own condition is `== 27`)
    assert_eq!(start_up(&script, &story(40)).ambient(loops).iter().map(|a| a.id).collect::<Vec<_>>(), [38]);
}

#[test]
fn a_stop_empties_a_channel_and_a_one_shot_replaces_a_loop_without_being_ambient() {
    let init = cat(&[play(1, 95), play(2, 38), play(3, 60), stop(1), play(2, 21)]);
    let bytes = section(vec![director(&init, &[])]);
    let ambient = start_up(&parsed(&bytes), &story(1)).ambient(loops);
    assert_eq!(ambient.iter().map(|a| (a.channel, a.id)).collect::<Vec<_>>(), [(3, 60)], "1 stopped, 2 replaced by a one-shot (21), 3 left on");
}

#[test]
fn what_the_main_part_starts_after_a_wait_starts_after_it_and_after_the_init_part() {
    let init = cat(&[wait(10), play(1, 95)]);
    let main = cat(&[wait(30), play(2, 242)]);
    let bytes = section(vec![director(&init, &main)]);
    let start = start_up(&parsed(&bytes), &story(1));
    let ambient = start.ambient(loops);
    assert_eq!(ambient.iter().map(|a| (a.channel, a.id, a.at)).collect::<Vec<_>>(), [(1, 95, 10), (2, 242, 40)], "the main part begins when the init part has returned (10 frames)");
    assert_eq!(start.stops().get("returned"), Some(&2));
}

#[test]
fn a_loop_whose_channel_a_part_the_run_did_not_reach_could_change_is_not_ambient_and_one_it_cannot_is() {
    // main: start 95 on channel 1, then a condition the story does not carry, then a stop of channel 1 and a play on 2
    let main = cat(&[play(1, 95), play(3, 38), when_byte(5, 1, 5, 11, &cat(&[stop(1)])), play(2, 242)]);
    let bytes = section(vec![director(&[], &main)]);
    let start = start_up(&parsed(&bytes), &story(1));
    assert_eq!(start.unjudged.len(), 1);
    let ambient = start.ambient(loops);
    assert_eq!(ambient.iter().map(|a| (a.channel, a.id)).collect::<Vec<_>>(), [(3, 38)], "channel 1 may be stopped by the unreached part, 3 cannot");
    assert_eq!(start.ambient_left_open(loops), 1);
    // an idle loop whose body only plays on channel 2 leaves the loops of channels 1 and 3 alone
    let main = cat(&[play(1, 95), play(3, 38), forever(&cat(&[wait(30), play(2, 9)]))]);
    let bytes = section(vec![director(&[], &main)]);
    let ambient = start_up(&parsed(&bytes), &story(1)).ambient(loops);
    assert_eq!(ambient.iter().map(|a| (a.channel, a.id)).collect::<Vec<_>>(), [(1, 95), (3, 38)]);
}

#[test]
fn every_entitys_script_zero_runs_and_the_last_play_on_a_channel_wins() {
    let bytes = section(vec![director(&play(1, 95), &[]), ("snd", vec![(0, cat(&[play(1, 242), ret()]))]), ("x", vec![])]);
    let ambient = start_up(&parsed(&bytes), &story(1)).ambient(loops);
    assert_eq!(ambient.iter().map(|a| (a.channel, a.id)).collect::<Vec<_>>(), [(1, 242)]);
}

#[test]
fn a_start_up_that_plays_nothing_is_empty_and_a_decode_failure_is_not_a_panic() {
    let bytes = section(vec![("a", vec![(0, ret())])]);
    let start = start_up(&parsed(&bytes), &story(1));
    assert!(start.events.is_empty() && start.ambient(loops).is_empty() && start.unjudged.is_empty());
    // an opcode that is no opcode: the script stops decoding there and the run says so
    let bytes = section(vec![("a", vec![(0, vec![0xEE, 0xEE, 0x00])])]);
    let start = start_up(&parsed(&bytes), &story(1));
    assert!(start.events.is_empty());
}

// ---- links, flips and exits ----------------------------------------------------------------------

/// `n` triangles side by side (triangle `i` spans x from 100 i to 100 i + 90, y from 0 to 90, at height 5).
fn mesh(n: i16) -> Walkmesh {
    let tri = |i: i16| {
        let o = i * 100;
        [Vertex { x: o, y: 0, z: 5 }, Vertex { x: o + 90, y: 0, z: 5 }, Vertex { x: o, y: 90, z: 5 }]
    };
    Walkmesh { triangles: (0..n).map(tri).collect(), access: vec![[0xFFFF; 3]; usize::try_from(n).unwrap()] }
}

/// The player (entity 0, with `player`'s scripts) and line entities (each: init part, other scripts).
fn field_with_lines(player: Scripts, lines: Vec<(Vec<u8>, Scripts)>) -> Vec<u8> {
    let mut hero = vec![(0, cat(&[vec![0xA1, 0x00], vec![0xA0, 0], ret()]))];
    hero.extend(player);
    let mut script = SynthScript { model_count: 1, scale: 512, ..Default::default() }.entity("hero", hero);
    for (n, (init, mut scripts)) in lines.into_iter().enumerate() {
        scripts.push((0, cat(&[init, ret()])));
        script = script.entity(&format!("line{n}"), scripts);
    }
    script.build()
}

#[test]
fn a_ladder_and_a_jump_play_what_the_players_own_scripts_play_with_the_times_the_scripts_give() {
    // the jump (walk-on, the player's script 3) and the ladder (OK, the player's script 4), in the shape of nmkin_3
    let jump = cat(&[dfanm(), wait(15), sound(62, 64), jump_to(110, 10, 1), wait(20), ret()]);
    let ladder = cat(&[sound(21, 64), move_to(205, 10), lader(210, 20, 5, 2), sound(62, 64), ret()]);
    let silent = cat(&[jump_to(10, 10, 0), ret()]);
    // an animation that is waited for comes before the sound: the data have the sound and no time for it
    let slow = cat(&[canm1(), wait(15), sound(62, 64), jump_to(110, 10, 1), ret()]);
    let bytes = field_with_lines(
        vec![(3, jump), (4, ladder), (5, silent), (6, slow)],
        vec![
            (line((-10, 0), (-10, 50), 7), vec![(2, cat(&[reqew(0, 3), ret()]))]),
            (line((300, 0), (300, 50), 9), vec![(4, cat(&[when_key(0x220, &reqew(0, 4)), ret()]))]),
            (line((500, 0), (500, 50), 9), vec![(2, cat(&[reqew(0, 5), ret()]))]),
            (line((700, 0), (700, 50), 9), vec![(2, cat(&[reqew(0, 6), ret()]))]),
        ],
    );
    let found = link_events(&parsed(&bytes), &mesh(3), &story(13));
    let [jumping, climbing, quiet, untimed] = found.as_slice() else { panic!("{found:?}") };
    assert!(quiet.events.is_empty() && !quiet.withheld, "a link that plays nothing is in the list, with nothing: {quiet:?}");
    assert!(untimed.events.is_empty() && untimed.withheld, "a sound with no time to give it is withheld, not guessed: {untimed:?}");
    assert!(!jumping.withheld && !climbing.withheld);
    assert!(!jumping.needs_ok && climbing.needs_ok);
    assert_eq!(plays(&jumping.events), [(15, 1, 62)], "the take-off half a second in");
    assert!(matches!(jumping.stop, Stop::Unknown { opcode: 0x03, .. }), "the line script waits for the player's: {:?}", jumping.stop);
    assert!(matches!(jumping.runs[1].stop, Stop::Unknown { opcode: 0xC0, .. }), "the player's script stops at the jump: {:?}", jumping.runs[1].stop);
    assert_eq!(plays(&climbing.events), [(0, 1, 21)], "the foot of the ladder at once; the sound after the climb has no known time");
    assert!(matches!(climbing.runs[1].stop, Stop::Unknown { opcode: 0xA8, .. }), "the player's script stops at the first walk: {:?}", climbing.runs[1].stop);
    // the identity the client matches the area's own link by
    assert_eq!(jumping.line, (Pos::new(-10.0, 0.0, 7.0), Pos::new(-10.0, 50.0, 7.0)));
    assert_eq!((jumping.to.x, jumping.to.y, jumping.to_hint), (110.0, 10.0, Some(1)));
    assert_eq!((climbing.to.x, climbing.to.y, climbing.to_hint), (210.0, 20.0, Some(2)));
}

#[test]
fn a_flipped_tracked_bit_plays_what_follows_it_and_an_untracked_bit_plays_nothing() {
    // the lift: the panel sets the bit, starts the hum, scrolls (a wait the data do not give), stops the hum
    let ride = cat(&[biton(1, 225, 0), sound(42, 64), scrlw(), sound(0, 64), ret()]);
    let back = cat(&[bitoff(1, 225, 0), sound(42, 64), scrlw(), sound(0, 64), ret()]);
    // bit 3 of the same byte is not carried by the story, and Var[1][9] is not tracked at all
    let other = cat(&[biton(1, 225, 3), sound(7, 64), biton(1, 9, 0), sound(8, 64), ret()]);
    let bytes = section(vec![("ele", vec![(3, ride), (4, back), (5, other)])]);
    let found = flip_events(&parsed(&bytes), &story(12));
    let [on, off] = found.as_slice() else { panic!("{found:?}") };
    assert_eq!((on.bank, on.address, on.bit, on.on, on.by.slot), (1, 225, 0, true, 3));
    assert_eq!((off.on, off.by.slot), (false, 4));
    for flip in [on, off] {
        assert_eq!(plays(&flip.events), [(0, 1, 42)], "the hum starts as the bit flips; the stop is behind a scroll nobody times");
        assert!(matches!(flip.stop, Stop::Unknown { opcode: 0x67, .. }), "{:?}", flip.stop);
    }
}

fn maplist(names: &[&str]) -> Maplist {
    let mut bytes = u16::try_from(names.len()).unwrap().to_le_bytes().to_vec();
    for name in names {
        let mut record = name.as_bytes().to_vec();
        record.resize(32, 0);
        bytes.extend(record);
    }
    Maplist::parse(&bytes).unwrap()
}

#[test]
fn a_door_plays_what_its_script_plays_before_the_jump_and_only_for_the_exit_the_story_opens() {
    // a line (slot 4, "Go") that, before the moment 9, plays 55 and jumps to field 1 and, from it on, plays 98 and jumps to field 2
    let go = cat(&[if_moment(3, 9, &cat(&[sound(55, 64), mapjump(1, 10, 20, 3)]), &cat(&[sound(98, 64), mapjump(2, 30, 40, 4)])), ret()]);
    let bytes = section(vec![("hero", vec![(0, cat(&[vec![0xA1, 0x00], vec![0xA0, 0], ret()]))]), ("door", vec![(0, cat(&[line((0, 0), (0, 50), 5), ret()])), (4, go)])]);
    let maplist = maplist(&["here", "md1_1", "md1_2"]);
    let early = exit_events(&parsed(&bytes), &maplist, &story(5));
    let [one] = early.as_slice() else { panic!("{early:?}") };
    assert_eq!((one.target.as_str(), one.arrival, one.triangle), ("md1_1", (10, 20), 3));
    assert_eq!(plays(&one.events), [(0, 1, 55)]);
    assert!(matches!(one.stop, Stop::LeavesField { .. }));
    let late = exit_events(&parsed(&bytes), &maplist, &story(20));
    let [two] = late.as_slice() else { panic!("{late:?}") };
    assert_eq!((two.target.as_str(), plays(&two.events)), ("md1_2", vec![(0, 1, 98)]));
    assert_eq!(two.line, Some(((0, 0), (0, 50))));
}
