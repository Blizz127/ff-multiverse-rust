//! Tests of the link scan on synthetic fields (never real game data).

use super::*;
use crate::field::Vertex;
use crate::script::synth::SynthScript;

// ---- a tiny assembler ----------------------------------------------------------------------

fn cat(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.concat()
}
fn ret() -> Vec<u8> {
    vec![0x00]
}
/// `REQEW entity script`.
fn reqew(entity: u8, slot: u8) -> Vec<u8> {
    vec![0x03, entity, slot]
}
/// `LINE` between two points at height `z`.
fn line(a: (i16, i16), b: (i16, i16), z: i16) -> Vec<u8> {
    let mut v = vec![0xD0];
    for c in [a.0, a.1, z, b.0, b.1, z] {
        v.extend_from_slice(&c.to_le_bytes());
    }
    v
}
/// `LINON enabled`.
fn linon(on: u8) -> Vec<u8> {
    vec![0xD1, on]
}
/// `IFKEYON keys` guarding `body`.
fn when_key(keys: u16, body: &[u8]) -> Vec<u8> {
    let mut v = vec![0x31];
    v.extend(keys.to_le_bytes());
    v.push(u8::try_from(body.len() + 1).unwrap());
    v.extend_from_slice(body);
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
/// `LADER x y z triangle` (arbitrary way, animation, direction and speed).
fn lader(x: i16, y: i16, z: i16, triangle: u16) -> Vec<u8> {
    let mut v = vec![0xC2, 0, 0];
    v.extend(x.to_le_bytes());
    v.extend(y.to_le_bytes());
    v.extend(z.to_le_bytes());
    v.extend(triangle.to_le_bytes());
    v.extend([1, 5, 64, 1]);
    v
}
/// `MOVE x y`.
fn move_to(x: i16, y: i16) -> Vec<u8> {
    let mut v = vec![0xA8, 0];
    v.extend(x.to_le_bytes());
    v.extend(y.to_le_bytes());
    v
}
/// `SETWORD Var[bank][addr] = value`.
fn setword(bank: u8, addr: u8, value: i16) -> Vec<u8> {
    let v = value.to_le_bytes();
    vec![0x81, bank << 4, addr, v[0], v[1]]
}
/// `GETAI entity -> Var[6][addr]`.
fn getai(entity: u8, addr: u8) -> Vec<u8> {
    vec![0xB9, 0x06, entity, addr]
}
/// `IFSW Var[lb][la] <op> Var[rb][ra]` guarding `body`.
fn when_vars(lb: u8, la: u8, rb: u8, ra: u8, op: u8, body: &[u8]) -> Vec<u8> {
    let mut v = vec![0x16, (lb << 4) | rb, la, 0, ra, 0, op, u8::try_from(body.len() + 1).unwrap()];
    v.extend_from_slice(body);
    v
}
/// `IFSW Var[bank][addr] <op> literal` guarding `body`.
fn when_lit(bank: u8, addr: u8, op: u8, literal: i16, body: &[u8]) -> Vec<u8> {
    let l = literal.to_le_bytes();
    let mut v = vec![0x16, bank << 4, addr, 0, l[0], l[1], op, u8::try_from(body.len() + 1).unwrap()];
    v.extend_from_slice(body);
    v
}

/// `n` triangles side by side (triangle `i` spans x from 100 i to 100 i + 90, y from 0 to 90, at height 5).
fn mesh(n: i16) -> Walkmesh {
    let tri = |i: i16| {
        let o = i * 100;
        [Vertex { x: o, y: 0, z: 5 }, Vertex { x: o + 90, y: 0, z: 5 }, Vertex { x: o, y: 90, z: 5 }]
    };
    Walkmesh { triangles: (0..n).map(tri).collect(), access: vec![[0xFFFF; 3]; usize::try_from(n).unwrap()] }
}

/// An entity's scripts as `(slot, code)`.
type Scripts = Vec<(usize, Vec<u8>)>;

/// The player entity (entity 0, character 0) with the scripts `player`, and `lines`: the entities after it, each with
/// its init part and its other scripts.
fn field(player: Scripts, lines: Vec<(Vec<u8>, Scripts)>) -> Vec<u8> {
    let mut hero = vec![(0, cat(&[vec![0xA1, 0x00], vec![0xA0, 0], ret()]))];
    hero.extend(player);
    let mut script = SynthScript { model_count: 1, scale: 512, ..Default::default() }.entity("hero", hero);
    for (n, (init, mut scripts)) in lines.into_iter().enumerate() {
        scripts.push((0, cat(&[init, ret()])));
        script = script.entity(&format!("line{n}"), scripts);
    }
    script.build()
}

fn scan(section: &[u8], triangles: i16) -> LinkScan {
    scan_links(&FieldScript::parse(section).unwrap(), &mesh(triangles))
}

// ---- the tests -------------------------------------------------------------------------------

#[test]
fn a_line_that_jumps_the_player_on_walking_and_one_that_climbs_on_ok_are_the_two_kinds_of_link() {
    let section = field(
        vec![(3, cat(&[jump_to(110, 10, 1), ret()])), (4, cat(&[move_to(205, 10), lader(210, 20, 5, 2), ret()]))],
        vec![
            // walked on: the player's script 3
            (line((-10, 0), (-10, 50), 7), vec![(2, cat(&[reqew(0, 3), ret()]))]),
            // OK: the line polls the key and requests the player's script 4
            (line((300, 0), (300, 50), 9), vec![(4, cat(&[when_key(0x220, &reqew(0, 4)), ret()]))]),
        ],
    );
    let scan = scan(&section, 3);
    assert_eq!(scan.dropped, []);
    let [jump, climb] = scan.found.as_slice() else { panic!("{:?}", scan.found) };
    assert_eq!((jump.entity, jump.slot, jump.needs_ok, jump.kind), (1, 2, false, LinkKind::Jump));
    assert_eq!(jump.line, (Pos::new(-10.0, 0.0, 7.0), Pos::new(-10.0, 50.0, 7.0)), "the line, with its height");
    assert_eq!((jump.to.x, jump.to.y, jump.triangle), (110.0, 10.0, 1));
    assert_eq!((climb.entity, climb.slot, climb.needs_ok, climb.kind), (2, 4, true, LinkKind::Ladder));
    assert_eq!((climb.to.x, climb.to.y, climb.to.z, climb.triangle), (210.0, 20.0, 5.0, 2), "the climb's end, not the walk before it");
    // as the adapter API offers them
    let link = climb.link();
    assert_eq!((link.needs_ok, link.to_hint, link.label.as_str()), (true, Some(2), "ladder"));
    assert_eq!(jump.link().label, "jump");
    assert!(jump.describe().contains("jump of line entity 1 (script 2), on walking up to triangle 1"), "{}", jump.describe());
}

#[test]
fn a_read_back_and_a_follow_up_jump_decide_the_destination_from_the_constants_the_field_sets() {
    // the director (entity 2) sets Var[6][2] = 2 in its init part; the climb ends on triangle 2, GETAI reads it, and a
    // jump follows when it is NOT the triangle the director expects
    let climb = |ends_on: u16| cat(&[lader(210, 20, 5, ends_on), getai(0, 0), when_vars(6, 0, 6, 2, 1, &jump_to(110, 10, 1)), ret()]);
    let director = (setword(6, 2, 2), Vec::new());
    let walked_on = (line((-10, 0), (-10, 50), 7), vec![(2, cat(&[reqew(0, 3), ret()]))]);
    // ends on the expected triangle: no follow-up
    let section = field(vec![(3, climb(2))], vec![director.clone(), walked_on.clone()]);
    let first = scan(&section, 3);
    assert_eq!(first.dropped, []);
    assert_eq!(first.found.iter().map(|l| (l.to.x, l.to.y, l.triangle, l.kind)).collect::<Vec<_>>(), [(210.0, 20.0, 2, LinkKind::Ladder)]);
    // ends on another: the follow-up jump is the destination (and the kind still says there was a climb)
    let section = field(vec![(3, climb(0))], vec![director, walked_on.clone()]);
    let scanned = scan(&section, 3);
    assert_eq!(scanned.found.iter().map(|l| (l.to.x, l.to.y, l.triangle, l.kind)).collect::<Vec<_>>(), [(110.0, 10.0, 1, LinkKind::Ladder)]);
    // without the director's constant nothing seeds the comparison: dropped, not guessed
    let section = field(vec![(3, climb(2))], vec![walked_on]);
    let scanned = scan(&section, 3);
    assert!(scanned.found.is_empty());
    assert_eq!(scanned.dropped, [(1, 2, "reads a variable nothing here knows")]);
}

#[test]
fn what_cannot_be_judged_is_dropped_with_its_reason_and_what_moves_nobody_is_no_link() {
    let walked_on = |slot_script: Vec<u8>| (line((-10, 0), (-10, 50), 7), vec![(2, cat(&[slot_script, ret()]))]);
    let player = |code: Vec<u8>| vec![(3, cat(&[code, ret()]))];
    let to_player = reqew(0, 3);
    // reads the story: where it moves the player (the moment is 0 here) and where it does not, it is not a link
    let moves_at_0 = field(player(when_lit(2, 0, 3, 5, &jump_to(110, 10, 1))), vec![walked_on(to_player.clone())]);
    assert_eq!(scan(&moves_at_0, 3).dropped, [(1, 2, "reads a variable nothing here knows")]);
    assert!(scan(&moves_at_0, 3).found.is_empty());
    let moves_later = field(player(when_lit(2, 0, 2, 5, &jump_to(110, 10, 1))), vec![walked_on(to_player.clone())]);
    assert_eq!(scan(&moves_later, 3).dropped, [(1, 2, "moves nobody now and reads a variable nothing here knows")]);
    // a read-back after a MOVE: the height is not known
    let unknown = field(player(cat(&[move_to(110, 10), vec![0xC1, 0x66, 0x66, 0, 7, 9, 11, 0], jump_to(110, 10, 1)])), vec![walked_on(to_player.clone())]);
    assert_eq!(scan(&unknown, 3).dropped, [(1, 2, "reads back a place the walk cannot know")]);
    // no triangle at the end (MOVE only)
    let no_triangle = field(player(move_to(110, 10)), vec![walked_on(to_player.clone())]);
    assert_eq!(scan(&no_triangle, 3).dropped, [(1, 2, "ends where no triangle of the walkmesh is named")]);
    // a triangle the walkmesh does not have
    let no_such = field(player(jump_to(110, 10, 9)), vec![walked_on(to_player.clone())]);
    assert_eq!(scan(&no_such, 3).dropped, [(1, 2, "ends where no triangle of the walkmesh is named")]);
    // a place outside the triangle named
    let outside = field(player(jump_to(180, 80, 1)), vec![walked_on(to_player.clone())]);
    assert_eq!(scan(&outside, 3).dropped, [(1, 2, "ends outside the triangle it names")]);
    // a map jump and a battle are no ladders
    let map_jump = field(player(cat(&[jump_to(110, 10, 1), vec![0x60, 0, 1, 0, 0, 0, 0, 0, 0, 0]])), vec![walked_on(to_player.clone())]);
    assert!(scan(&map_jump, 3).found.is_empty());
    let battle = field(player(cat(&[jump_to(110, 10, 1), vec![0x70, 0, 5, 0]])), vec![walked_on(to_player.clone())]);
    assert!(scan(&battle, 3).found.is_empty());
    // a line whose script moves nobody (or asks for no script that moves anybody)
    let idle = field(player(jump_to(110, 10, 1)), vec![walked_on(Vec::new())]);
    let scanned = scan(&idle, 3);
    assert!(scanned.found.is_empty() && scanned.dropped.is_empty());
    // a character that is not a player is not the player
    let npc_moves = SynthScript { model_count: 1, scale: 512, ..Default::default() }
        .entity("hero", vec![(0, cat(&[vec![0xA1, 0x00], vec![0xA0, 0], ret()]))])
        .entity("npc", vec![(0, cat(&[vec![0xA1, 0x01], ret()])), (3, cat(&[jump_to(110, 10, 1), ret()]))])
        .entity("line", vec![(0, cat(&[line((-10, 0), (-10, 50), 7), ret()])), (2, cat(&[reqew(1, 3), ret()]))])
        .build();
    assert!(scan(&npc_moves, 3).found.is_empty());
}

#[test]
fn a_line_that_starts_disabled_is_no_link_and_a_line_switched_on_is() {
    let player = vec![(3, cat(&[jump_to(110, 10, 1), ret()]))];
    let linked = |init: Vec<u8>| field(player.clone(), vec![(init, vec![(2, cat(&[reqew(0, 3), ret()]))])]);
    let l = line((-10, 0), (-10, 50), 7);
    assert!(scan(&linked(cat(&[l.clone(), linon(0)])), 3).found.is_empty());
    assert_eq!(scan(&linked(cat(&[l.clone(), linon(1)])), 3).found.len(), 1);
    assert_eq!(scan(&linked(l), 3).found.len(), 1, "a line that never says is on");
}

#[test]
fn a_script_that_moves_the_player_whether_or_not_ok_is_pressed_is_a_walk_on_link() {
    // slot 4 is where the ladders poll the key, but a slot 4 that does not ask for it moves the player when walked on
    let player = vec![(3, cat(&[jump_to(110, 10, 1), ret()]))];
    let always = field(player, vec![(line((-10, 0), (-10, 50), 7), vec![(4, cat(&[reqew(0, 3), ret()]))])]);
    let scanned = scan(&always, 3);
    assert_eq!(scanned.found.len(), 1);
    assert!(scanned.found.iter().all(|l| !l.needs_ok));
}

#[test]
fn damaged_and_random_scripts_never_panic_the_scan() {
    let base = field(
        vec![(3, cat(&[jump_to(110, 10, 1), lader(210, 20, 5, 2), vec![0xC1, 0x66, 0x66, 0, 7, 9, 11, 0], ret()]))],
        vec![(line((-10, 0), (-10, 50), 7), vec![(2, cat(&[reqew(0, 3), ret()])), (4, cat(&[when_key(0x220, &reqew(0, 3)), ret()]))])],
    );
    let mut state = 0x1234_5678_9ABC_DEF1u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..2000 {
        let mut bytes = base.clone();
        for _ in 0..(1 + next() % 5) {
            let at = usize::try_from(next()).unwrap() % bytes.len();
            match next() % 3 {
                0 => bytes[at] = next().to_le_bytes()[0],
                1 => bytes.truncate(at.max(1)),
                _ => bytes.push(next().to_le_bytes()[0]),
            }
        }
        if let Ok(script) = FieldScript::parse(&bytes) {
            let _ = scan_links(&script, &mesh(3));
        }
    }
}
