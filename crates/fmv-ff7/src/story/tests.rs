use super::*;
use crate::field::Vertex;
use crate::script::dialogue::StartOfGame;
use crate::script::synth::SynthScript;

// ---- a tiny assembler for the opcodes the story reads ----

fn ret() -> Vec<u8> {
    vec![0x00]
}
/// `SETWORD Var[bank][addr] = value`.
fn setword(bank: u8, addr: u8, value: i16) -> Vec<u8> {
    let v = value.to_le_bytes();
    vec![0x81, bank << 4, addr, v[0], v[1]]
}
/// `BITON Var[bank][addr] bit` / `BITOFF`.
fn biton(bank: u8, addr: u8, bit: u8) -> Vec<u8> {
    vec![0x82, bank << 4, addr, bit]
}
fn bitoff(bank: u8, addr: u8, bit: u8) -> Vec<u8> {
    vec![0x83, bank << 4, addr, bit]
}
/// `GETAI entity -> Var[bank][addr]`.
fn getai(entity: u8, bank: u8, addr: u8) -> Vec<u8> {
    vec![0xB9, bank, entity, addr]
}
/// The distance a forward jump needs to skip `n` bytes after its own instruction (it counts from its operand byte).
fn skip(n: usize) -> u8 {
    u8::try_from(n + 1).unwrap()
}
/// `IFSW Var[lb][la] <op> Var[rb][ra]` (a variable on each side); `jump` is counted from the jump
/// operand (the last byte) and taken when the comparison is false.
fn ifsw_vars(lb: u8, la: u8, rb: u8, ra: u8, op: u8, jump: u8) -> Vec<u8> {
    vec![0x16, (lb << 4) | rb, la, 0, ra, 0, op, jump]
}
/// `IFSW Var[bank][addr] <op> literal`.
fn ifsw_lit(bank: u8, addr: u8, literal: i16, op: u8, jump: u8) -> Vec<u8> {
    let l = literal.to_le_bytes();
    vec![0x16, bank << 4, addr, 0, l[0], l[1], op, jump]
}
/// `IFUB Var[bank][addr] <op> literal`.
fn ifub_lit(bank: u8, addr: u8, literal: u8, op: u8, jump: u8) -> Vec<u8> {
    vec![0x14, bank << 4, addr, literal, op, jump]
}
fn ifkeyon(keys: u16, jump: u8) -> Vec<u8> {
    let k = keys.to_le_bytes();
    vec![0x31, k[0], k[1], jump]
}
fn concat(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.concat()
}
/// `body` under `IFSW Var[2][0] <op> literal`.
fn when_moment(op: u8, literal: i16, body: &[u8]) -> Vec<u8> {
    concat(&[ifsw_lit(2, 0, literal, op, skip(body.len())), body.to_vec()])
}
/// `body` under a test of bit `bit` of Var[1][225]: `op` 9 when it is set, 10 when it is clear.
fn when_lift_bit(op: u8, bit: u8, body: &[u8]) -> Vec<u8> {
    concat(&[ifub_lit(1, 225, bit, op, skip(body.len())), body.to_vec()])
}
/// `body` under `IFSW Var[6][0] == triangle` (the variable the director's loop fills with the player's triangle).
fn when_on_triangle(triangle: i16, body: &[u8]) -> Vec<u8> {
    concat(&[ifsw_lit(6, 0, triangle, 0, skip(body.len())), body.to_vec()])
}
fn when_ok_pressed(body: &[u8]) -> Vec<u8> {
    concat(&[ifkeyon(0x220, skip(body.len())), body.to_vec()])
}
/// `LINE` between two points (all z 7).
fn line(a: (i16, i16), b: (i16, i16)) -> Vec<u8> {
    let mut v = vec![0xD0];
    for c in [a.0, a.1, 7, b.0, b.1, 7] {
        v.extend_from_slice(&c.to_le_bytes());
    }
    v
}
/// A loop: `body`, then a jump back to its first byte.
fn looping(body: &[u8]) -> Vec<u8> {
    concat(&[body.to_vec(), vec![0x12, u8::try_from(body.len()).unwrap()]])
}

/// `n` triangles side by side (triangle `i` at x = 100 i).
fn mesh(n: usize) -> Walkmesh {
    let tri = |i: i16| {
        let o = i * 100;
        [Vertex { x: o, y: 0, z: 5 }, Vertex { x: o + 90, y: 0, z: 5 }, Vertex { x: o, y: 90, z: 5 }]
    };
    let count = i16::try_from(n).unwrap();
    Walkmesh { triangles: (0..count).map(tri).collect(), access: vec![[0xFFFF; 3]; n] }
}

/// The first mission's pattern: `dir` runs the loop, `hero` is the entity it asks about and is
/// bound to the playable character `pc` (`None`: bound to none).
fn field_script(set_to: i16, moment_op: u8, moment_lit: i16, pc: Option<u8>) -> Vec<u8> {
    // The loop: GETAI, two IFSW that skip to the JMPB at its end, the write, JMPB back to the top.
    let tail = concat(&[setword(2, 0, set_to), vec![0xBF, 0x01]]); // SETWORD, then CC (any instruction)
    let second = ifsw_lit(2, 0, moment_lit, moment_op, u8::try_from(tail.len() + 1).unwrap());
    let first = ifsw_vars(6, 4, 6, 9, 0, u8::try_from(second.len() + tail.len() + 1).unwrap());
    let top = getai(1, 6, 4);
    let jmpb = vec![0x12, u8::try_from(top.len() + first.len() + second.len() + tail.len()).unwrap()];
    let init = concat(&[setword(6, 9, 12), ret()]);
    let director = concat(&[init, top, first, second, tail, jmpb]);
    let hero = match pc {
        Some(c) => concat(&[vec![0xA1, 0x00], vec![0xA0, c], ret()]),
        None => concat(&[vec![0xA1, 0x00], ret()]),
    };
    SynthScript { model_count: 1, scale: 512, ..Default::default() }.entity("dir", vec![(0, director)]).entity("hero", vec![(0, hero)]).build()
}

/// A field with one player entity (`hero`, character 0) whose triangle the director keeps in `Var[6][0]`, plus `others`.
fn field_with_player(others: impl FnOnce(SynthScript) -> SynthScript) -> Vec<u8> {
    let director = concat(&[ret(), looping(&getai(1, 6, 0))]);
    let hero = concat(&[vec![0xA1, 0x00], vec![0xA0, 0], ret()]);
    let script = SynthScript { model_count: 1, scale: 512, ..Default::default() }.entity("dir", vec![(0, director)]).entity("hero", vec![(0, hero)]);
    others(script).build()
}

/// An entity whose script 0 has the init part `init` and the main part `main` (which does not loop by itself).
fn entity_script(init: &[u8], main: &[u8]) -> Vec<u8> {
    concat(&[init.to_vec(), ret(), main.to_vec()])
}

fn scan(section: &[u8], mesh: &Walkmesh) -> AreaStory {
    AreaStory::scan(&FieldScript::parse(section).unwrap(), mesh)
}

fn at(moment: u16) -> Vec<u8> {
    StoryState::at(moment).encode()
}

/// The state with the lift bit set (what the elevator's ride leaves).
fn lifted(moment: u16) -> StoryState {
    let mut s = StoryState::at(moment);
    assert!(s.set_bit(1, 225, 0, true));
    s
}

// ---- the blob ----

#[test]
fn the_empty_blob_is_a_new_account_and_a_stored_moment_round_trips() {
    assert_eq!(StoryState::decode(&[]), Some(StoryState::new_game()));
    assert_eq!(StoryState::new_game().moment, NEW_GAME_MOMENT);
    for moment in [0u16, 1, 6, 255, 256, 1600, u16::MAX] {
        let blob = at(moment);
        assert_eq!(blob.len(), 3);
        assert_eq!(StoryState::decode(&blob), Some(StoryState::at(moment)));
    }
    // What is not exactly version 1 or a whole version 2 is not read: other versions, other lengths, the old test blobs.
    for bad in [&[7u8, 7][..], &[2, 1, 0], &[1, 1], &[1, 1, 0, 0], &[0], &[0xAB; 64], &[3, 1, 0, 1, 1, 225, 1]] {
        assert_eq!(StoryState::decode(bad), None, "{bad:?}");
    }
}

/// What a version 1 build wrote (`[1, lo, hi]`) loads exactly as before, with no variable, and writes back as itself.
#[test]
fn a_version_1_blob_loads_as_before_and_is_written_back_byte_for_byte() {
    for (blob, moment) in [(vec![1u8, 6, 0], 6u16), (vec![1, 7, 0], 7), (vec![1, 0x40, 0x06], 1600), (vec![1, 1, 0], 1)] {
        let state = StoryState::decode(&blob).unwrap();
        assert_eq!((state.moment, state.vars().count()), (moment, 0), "{blob:?}");
        assert_eq!(state.encode(), blob, "{blob:?} comes back as it was");
        assert_eq!(canon_party_of(&blob), vec![CLOUD, BARRET]);
    }
    // a state with no variable never grows a version 2 blob, however it was made
    let mut s = StoryState::at(12);
    assert!(s.set_bit(1, 225, 0, true) && s.set_bit(1, 225, 0, false));
    assert_eq!(s.encode(), vec![1, 12, 0]);
}

#[test]
fn a_version_2_blob_carries_the_moment_and_the_variables_in_one_canonical_form() {
    let state = lifted(12);
    let blob = state.encode();
    assert_eq!(blob, vec![2, 12, 0, 1, 1, 225, 1]);
    assert_eq!(StoryState::decode(&blob), Some(state.clone()));
    assert_eq!(state.var(1, 225), 1);
    assert_eq!(state.vars().collect::<Vec<_>>(), [((1, 225), 1)]);
    // a variable of another bank, in ascending order, is kept as it is written (a later build may carry more)
    let more = [2u8, 13, 0, 2, 1, 225, 1, 5, 9, 0x82];
    let decoded = StoryState::decode(&more).unwrap();
    assert_eq!((decoded.moment, decoded.var(5, 9)), (13, 0x82));
    assert_eq!(decoded.encode(), more, "decode then encode is the identity on what is canonical");
    // there is one way to write a state: everything else is not read
    for bad in [
        vec![2u8, 12, 0, 0],                         // no variable: that is version 1
        vec![2, 12, 0, 1, 1, 225, 0],                // a variable that is 0 is not written
        vec![2, 12, 0, 2, 5, 9, 1, 1, 225, 1],       // not ascending
        vec![2, 12, 0, 2, 1, 225, 1, 1, 225, 1],     // twice
        vec![2, 12, 0, 1, 0, 225, 1],                // bank 0 is a literal, not a variable
        vec![2, 12, 0, 1, 16, 225, 1],               // there are banks 1 to 15
        vec![2, 12, 0, 1, 2, 0, 1],                  // the moment is in the header
        vec![2, 12, 0, 1, 2, 1, 1],                  // and so is its second byte
        vec![2, 12, 0, 1, 1, 225],                   // cut short
        vec![2, 12, 0, 1, 1, 225, 1, 0],             // too long
        vec![2, 12, 0, 2, 1, 225, 1],                // says two, holds one
        concat(&[vec![2, 12, 0, 17], (0..17u8).flat_map(|i| [1, i, 1]).collect()]), // more than MAX_STORY_VARS
    ] {
        assert_eq!(StoryState::decode(&bad), None, "{bad:?}");
    }
    let sixteen: Vec<u8> = concat(&[vec![2, 12, 0, 16], (0..16u8).flat_map(|i| [1, i, 1]).collect()]);
    assert_eq!(StoryState::decode(&sixteen).map(|s| s.vars().count()), Some(MAX_STORY_VARS));
}

#[test]
fn only_the_tracked_bits_can_be_set_and_known() {
    let mut s = StoryState::new_game();
    assert_eq!(REACTOR_LIFT, tracked_var(1, 225).unwrap());
    assert!(!s.set_bit(1, 225, 7, true), "bit 7 of the reactor byte is not carried");
    assert!(!s.set_bit(1, 224, 0, true), "nor is another variable");
    assert!(!s.set_bit(1, 225, 8, true) && !s.set_bit(1, 225, 200, true), "there are eight bits");
    assert_eq!(s.vars().count(), 0);
    assert_eq!(s.known_bit(1, 225, 0), Some(false), "a new game has the lift up");
    assert_eq!(s.known_bit(1, 225, 7), None, "the bits it does not carry are unknown, not 0");
    assert_eq!(s.known_byte(1, 225), None, "a byte it carries only a bit of is not known whole");
    assert!(s.set_bit(1, 225, 0, true) && !s.set_bit(1, 225, 0, true), "setting twice changes once");
    assert_eq!((s.known_bit(1, 225, 0), s.var(1, 225)), (Some(true), 1));
    // the moment is known whole, as a word
    let m = StoryState::at(0x0102);
    assert_eq!((m.known_byte(2, 0), m.known_byte(2, 1), m.known_byte(2, 2)), (Some(2), Some(1), None));
    assert_eq!((m.known_word(2, 0), m.known_word(2, 1)), (Some(0x0102), None), "the word at address 1 would read the unknown byte at 2");
    assert_eq!(m.known_bit(2, 0, 1), Some(true));
    assert_eq!(m.known_bit(2, 0, 9), None);
    assert_eq!(m.known_byte(6, 0), None, "an untracked variable is unknown");
}

#[test]
fn an_accounts_story_is_the_dialogue_lanes_story_variables() {
    // Var[2][0] is the moment as a little-endian word; everything else is a new game's.
    for moment in [0u16, 1, 6, 255, 256, 1600, u16::MAX] {
        let state = StoryState::at(moment);
        let [lo, hi] = moment.to_le_bytes();
        assert_eq!((state.byte(2, 0), state.byte(2, 1)), (lo, hi), "moment {moment}");
        for (bank, address) in [(1u8, 0u8), (2, 2), (2, 255), (6, 0), (15, 32), (1, 225)] {
            assert_eq!(state.byte(bank, address), 0, "bank {bank} address {address}");
        }
    }
    // ...and what the story set in a tracked variable is read back by the same hook (the lift is down after the ride)
    assert_eq!(lifted(12).byte(1, 225), 1);
    assert_eq!(lifted(12).byte(1, 224), 0);
    let state = StoryState::new_game();
    assert_eq!((0..3).map(|s| state.party_member(s)).collect::<Vec<_>>(), (0..3).map(|s| StartOfGame.party_member(s)).collect::<Vec<_>>());
    assert_eq!(StoryState::decode(&[]).map(|s| s.byte(2, 0)), Some(1), "a new account reads moment 1");
}

/// The canon party table is a small contract: it covers every moment, ascends, and names at most three
/// distinct characters of the nine.
#[test]
fn the_canon_party_table_covers_every_moment_in_order() {
    assert_eq!(CANON_PARTIES.first().map(|row| row.from_moment), Some(0), "every moment has a party");
    assert!(CANON_PARTIES.windows(2).all(|w| w[0].from_moment < w[1].from_moment), "rows ascend by moment");
    for row in CANON_PARTIES {
        assert!(!row.members.is_empty() && row.members.len() <= 3, "{row:?}: FF7's party has three slots");
        assert!(row.members.iter().all(|&c| c <= 8), "{row:?}: a character id");
        let mut seen = row.members.to_vec();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), row.members.len(), "{row:?}: nobody twice");
    }
    for moment in [0u16, 1, 6, 7, 255, 1600, u16::MAX] {
        assert_eq!(canon_party(&StoryState::at(moment)), vec![CLOUD, BARRET], "moment {moment}");
    }
}

/// The first reactor mission is fought by Cloud and Barret, from the very start: the party the story has
/// always answered to a dialogue script's `IFPRTYQ`, for a new account (the empty blob), a stored moment and a
/// blob nobody can read.
#[test]
fn the_first_missions_canon_party_is_cloud_and_barret_for_every_blob() {
    let stored = StoryState::at(6).encode();
    let blobs: [&[u8]; 5] = [&[], &stored, &[9, 9], &[BLOB_VERSION, 1], &[3, 1, 0]];
    for blob in blobs {
        assert_eq!(canon_party_of(blob), vec![CLOUD, BARRET], "{blob:?}");
    }
    assert_eq!(canon_party_of(&lifted(12).encode()), vec![CLOUD, BARRET], "a version 2 blob has the party of its moment");
    assert_eq!(
        canon_party(&StoryState::new_game()).iter().map(|&c| Some(c)).collect::<Vec<_>>(),
        (0..2).map(|slot| StartOfGame.party_member(slot)).collect::<Vec<_>>(),
        "the table agrees with the new game's party the dialogue lane has always used"
    );
    // A table row is what a later moment reads: the lookup takes the last row that has begun.
    let state = StoryState::at(40);
    assert_eq!(canon_party(&state), CANON_PARTIES.iter().rev().find(|r| r.from_moment <= 40).unwrap().members.to_vec());
}

#[test]
fn merging_keeps_the_further_moment_and_never_replaces_what_it_cannot_read() {
    assert_eq!(merge_progression(&[], &at(6)), at(6), "a new account takes the advanced story");
    assert_eq!(merge_progression(&at(6), &at(7)), at(7));
    assert_eq!(merge_progression(&at(7), &at(6)), at(7), "a late, older save cannot take the story back");
    assert_eq!(merge_progression(&at(6), &at(6)), at(6));
    assert_eq!(merge_progression(&at(6), &[]), at(6), "the default is not further than anything");
    assert_eq!(merge_progression(&[], &at(1)), Vec::<u8>::new(), "an equal story leaves the stored blob as it is");
    assert_eq!(merge_progression(&[7, 7], &at(9)), vec![7, 7], "an unreadable record is never overwritten");
    assert_eq!(merge_progression(&at(9), &[7, 7]), at(9));
}

#[test]
fn merging_joins_the_variables_and_a_late_save_without_them_does_not_drop_them() {
    let rode = lifted(12).encode();
    // the first save of a variable over a stored moment-only story
    assert_eq!(merge_progression(&at(11), &rode), rode);
    assert_eq!(merge_progression(&[], &rode), rode, "even over a new account");
    // a late save from before the ride (version 1, an older moment, or even a further one) keeps the bit
    assert_eq!(merge_progression(&rode, &at(11)), rode, "stored bytes come back untouched when the join adds nothing");
    assert_eq!(merge_progression(&rode, &[]), rode);
    assert_eq!(merge_progression(&rode, &at(13)), lifted(13).encode(), "a further moment keeps the stored variables");
    // two stories that each have something the other lacks join
    let mut other = StoryState::at(11);
    other.vars.insert((5, 9), 0x82);
    assert_eq!(merge_progression(&rode, &other.encode()), {
        let mut joined = lifted(12);
        joined.vars.insert((5, 9), 0x82);
        joined.encode()
    });
    // the bits of one variable are united, never replaced
    let mut a = StoryState::at(5);
    a.vars.insert((5, 9), 0b0101);
    let mut b = StoryState::at(5);
    b.vars.insert((5, 9), 0b0011);
    let joined = StoryState::decode(&merge_progression(&a.encode(), &b.encode())).unwrap();
    assert_eq!(joined.var(5, 9), 0b0111);
    // an unreadable record is still never overwritten, in either place
    assert_eq!(merge_progression(&[7, 7], &rode), vec![7, 7]);
    assert_eq!(merge_progression(&rode, &[7, 7]), rode);
    // and the join cannot grow a blob past the cap
    let full = StoryState::decode(&concat(&[vec![2, 1, 0, 16], (0..16u8).flat_map(|i| [3, i, 1]).collect()])).unwrap();
    let mut more = StoryState::at(2);
    more.vars.insert((4, 0), 1);
    assert_eq!(StoryState::decode(&merge_progression(&full.encode(), &more.encode())).map(|s| (s.moment, s.vars().count())), Some((2, 16)));
}

// ---- phases ----

#[test]
fn the_intervals_between_the_literals_a_field_compares_are_its_phases() {
    // `moment == 1` cuts the axis at 1 and 2: the intervals are [0,1) [1,2) [2,..)
    let story = scan(&field_script(6, 0, 1, Some(0)), &mesh(13));
    assert_eq!(story.edges(), [1, 2]);
    let key = |m: u16| story.phase_key(&at(m));
    assert_eq!(key(1), Some(PhaseKey::default()), "the interval of the new-game moment is the empty key");
    assert_eq!(story.phase_key(&[]), Some(PhaseKey::default()), "a new account");
    assert_eq!(key(0), Some(PhaseKey(vec![PHASE_TAG, 0, 0])));
    assert_eq!(key(2), Some(PhaseKey(vec![PHASE_TAG, 2, 0])));
    assert_eq!(key(6), key(2), "6 and 2 look the same to this field");
    assert_eq!(key(9999), key(2));
    assert_eq!(story.phase_key(&[7, 7]), None, "a blob that is not ours has no phase of ours");
    // a version 2 blob is read for its moment: the variables take no part in phases
    assert_eq!(story.phase_key(&lifted(6).encode()), key(6));
    assert_eq!(story.phase_key(&lifted(1).encode()), Some(PhaseKey::default()));
}

#[test]
fn every_comparison_cuts_the_axis_where_its_answer_changes() {
    for (op, lit, cuts) in [(0u8, 5i16, vec![5u16, 6]), (1, 5, vec![5, 6]), (2, 5, vec![6]), (3, 5, vec![5]), (4, 5, vec![5]), (5, 5, vec![6])] {
        // a field with one comparison and nothing else
        let code = concat(&[ifsw_lit(2, 0, lit, op, 1), ret()]);
        let sec = SynthScript::default().entity("e", vec![(0, code)]).build();
        let story = scan(&sec, &mesh(1));
        assert_eq!(story.edges(), &cuts[..], "op {op}");
        // and two moments share a phase exactly when the comparison answers the same for both
        let cmp = Cmp::from_code(i32::from(op)).unwrap();
        let key = |x: u16| story.phase_key(&at(x)).unwrap();
        for m in 0..12u16 {
            let same_answer = cmp.holds(m, i32::from(lit)) == cmp.holds(m + 1, i32::from(lit));
            assert_eq!(key(m) == key(m + 1), same_answer, "op {op}, moments {m} and {}", m + 1);
        }
    }
    // the literal on the left means the comparison from the other side: `5 < moment` is `moment > 5`
    let flipped = vec![0x16, 0x02, 0x05, 0x00, 0x00, 0x00, 3, 1];
    let sec = SynthScript::default().entity("e", vec![(0, concat(&[flipped, ret()]))]).build();
    assert_eq!(scan(&sec, &mesh(1)).edges(), [6]);
}

#[test]
fn a_field_that_never_compares_the_counter_has_one_phase_for_everybody() {
    let sec = SynthScript::default().entity("e", vec![(0, ret())]).build();
    let story = scan(&sec, &mesh(1));
    assert!(story.edges().is_empty() && story.point_count() == 0);
    for blob in [vec![], at(0), at(900), lifted(12).encode()] {
        assert_eq!(story.phase_key(&blob), Some(PhaseKey::default()));
    }
}

// ---- story points: a triangle ----

#[test]
fn stepping_onto_the_triangle_of_the_loop_advances_a_player_whose_story_matches() {
    let story = scan(&field_script(6, 0, 1, Some(0)), &mesh(13));
    assert_eq!(story.point_count(), 1);
    let points = story.points();
    let point = points.first().unwrap();
    assert_eq!(point.id, 0);
    let StoryTrigger::Region(corners) = &point.trigger else { panic!("{point:?}") };
    assert_eq!(corners.len(), 3);
    assert_eq!((corners[0].x, corners[0].y), (1200.0, 0.0), "triangle 12 of the mesh");
    assert!(point.label.contains("triangle 12") && point.label.contains("moment ==1 -> 6"), "{}", point.label);
    assert_eq!(point.label, "triangle 12: moment ==1 -> 6", "the label of a moment-only point is what it always was");

    // A new account (empty blob) is at the moment the point needs: it advances to 6.
    let step = story.advance(&[], 0).expect("a new account advances");
    assert_eq!(step.progress, at(6));
    assert_eq!(step.summary, "moment=1->6");
    // Once advanced it cannot do it again, and nobody at another moment can.
    assert_eq!(story.advance(&step.progress, 0), None);
    for m in [0u16, 2, 5, 6, 7, 1600] {
        assert_eq!(story.advance(&at(m), 0), None, "moment {m}");
    }
    // An unknown point and an unreadable blob do nothing.
    assert_eq!(story.advance(&[], 1), None);
    assert_eq!(story.advance(&[7, 7], 0), None);
    // A version 2 blob advances too, and keeps its variables.
    let mut rode = StoryState::at(1);
    rode.set_bit(1, 225, 0, true);
    let step = story.advance(&rode.encode(), 0).expect("variables do not stop the moment");
    assert_eq!(StoryState::decode(&step.progress).map(|s| (s.moment, s.var(1, 225))), Some((6, 1)));
    assert_eq!(step.summary, "moment=1->6", "only what changed is said");
}

#[test]
fn a_lower_guard_such_as_less_than_works_the_same_and_a_story_never_goes_back() {
    // `moment < 7 -> 7` (the shape of the next field of the first mission)
    let story = scan(&field_script(7, 3, 7, Some(0)), &mesh(13));
    assert_eq!(story.point_count(), 1);
    assert_eq!(story.advance(&at(1), 0).unwrap().progress, at(7));
    assert_eq!(story.advance(&at(6), 0).unwrap().summary, "moment=6->7");
    assert_eq!(story.advance(&at(7), 0), None);
    // A point whose value is not higher than the moment never lowers it, whatever the guard says.
    let story = scan(&field_script(3, 3, 7, Some(0)), &mesh(13));
    assert_eq!(story.advance(&at(5), 0), None, "5 -> 3 would be going back");
    assert_eq!(story.advance(&at(1), 0).unwrap().progress, at(3));
}

#[test]
fn a_loop_that_is_not_the_pattern_makes_no_story_point() {
    // the entity the loop asks about is not bound to a playable character
    assert_eq!(scan(&field_script(6, 0, 1, None), &mesh(13)).point_count(), 0, "no PC: not the player");
    // the triangle is not in the walkmesh
    assert_eq!(scan(&field_script(6, 0, 1, Some(0)), &mesh(12)).point_count(), 0, "triangle 12 does not exist");
    // a guard the server cannot judge (a bit test on the counter instead of a comparison)
    assert_eq!(scan(&field_script(6, 6, 1, Some(0)), &mesh(13)).point_count(), 0);
}

#[test]
fn the_player_in_the_rule_is_any_playable_character_never_one_in_particular() {
    // The entity the loop asks about may stand for any character at all: the rule is "the player".
    for character in [0u8, 1, 5, 8, 10] {
        assert_eq!(scan(&field_script(6, 0, 1, Some(character)), &mesh(13)).point_count(), 1, "character {character}");
    }
}

#[test]
fn the_triangle_may_be_tested_against_a_literal_and_its_variable_filled_by_another_entitys_loop() {
    // the director fills Var[6][0] with the hero's triangle; the panel's own main loop asks `Var[6][0] == 3`
    let panel = entity_script(&[], &looping(&when_on_triangle(3, &when_moment(3, 9, &setword(2, 0, 9)))));
    let sec = field_with_player(|s| s.entity("panel", vec![(0, panel)]));
    let story = scan(&sec, &mesh(5));
    assert_eq!(story.point_count(), 1);
    let points = story.points();
    assert!(matches!(points[0].trigger, StoryTrigger::Region(_)) && points[0].label == "triangle 3: moment <9 -> 9", "{}", points[0].label);
    assert_eq!(story.advance(&at(4), 0).unwrap().summary, "moment=4->9");
    // the same test of a variable no player's triangle fills is not a region
    let stranger = entity_script(&[], &looping(&concat(&[ifsw_lit(6, 5, 3, 0, skip(when_moment(3, 9, &setword(2, 0, 9)).len())), when_moment(3, 9, &setword(2, 0, 9))])));
    let sec = field_with_player(|s| s.entity("stranger", vec![(0, stranger)]));
    assert_eq!(scan(&sec, &mesh(5)).point_count(), 0);
    // and a variable that a GETAI of someone who is no player also fills is no player's triangle either
    let other = entity_script(&[], &looping(&getai(2, 6, 0)));
    let panel = entity_script(&[], &looping(&when_on_triangle(3, &when_moment(3, 9, &setword(2, 0, 9)))));
    let sec = field_with_player(|s| s.entity("other", vec![(0, other)]).entity("panel", vec![(0, panel)]));
    assert_eq!(scan(&sec, &mesh(5)).point_count(), 0, "Var[6][0] is also somebody else's triangle there");
}

// ---- story points: the elevator ----

/// The elevator's panel: stand on triangle 8 and press OK; going down (the lift bit clear) turns the bit on, and the
/// first ride (moment below 12) also moves the story to 12.
fn the_ride() -> Vec<u8> {
    let down = concat(&[biton(1, 225, 0), when_moment(3, 12, &setword(2, 0, 12))]);
    let ride = when_on_triangle(8, &when_ok_pressed(&when_lift_bit(10, 0, &down)));
    entity_script(&[], &looping(&ride))
}

#[test]
fn the_ride_is_one_point_whose_writes_are_judged_together_on_the_story_as_it_was() {
    let sec = field_with_player(|s| s.entity("ele", vec![(0, the_ride())]));
    let story = scan(&sec, &mesh(9));
    assert_eq!(story.point_count(), 1, "the bit and the moment are one ride");
    let point = story.points().remove(0);
    let StoryTrigger::Region(corners) = &point.trigger else { panic!("{point:?}") };
    assert_eq!(corners.len(), 3);
    assert_eq!(
        point.label,
        "triangle 8 (OK): Var[1][225] bit 0 clear -> Var[1][225] bit 0 on; Var[1][225] bit 0 clear and moment <12 -> 12"
    );

    // the first ride of an account (it reached moment 11): the lift goes down and the story moves to 12, in one step
    let step = story.advance(&at(11), 0).expect("the ride");
    assert_eq!(StoryState::decode(&step.progress), Some(lifted(12)));
    assert_eq!(step.progress, vec![2, 12, 0, 1, 1, 225, 1], "a version 2 blob");
    assert_eq!(step.summary, "moment=11->12 var1.225=0->1");
    assert_eq!(story.advance(&[], 0).map(|s| s.summary).as_deref(), Some("moment=1->12 var1.225=0->1"), "a new account too: the story only moves forward");
    // having ridden, there is nothing more to do here: no ride back up (a bit that clears moves nothing forward)
    assert_eq!(story.advance(&step.progress, 0), None);
    assert_eq!(story.advance(&lifted(40).encode(), 0), None);
    // a story already past the first ride but with the lift up (not a state the model makes) gets the bit alone
    let step = story.advance(&at(20), 0).expect("the bit");
    assert_eq!(StoryState::decode(&step.progress), Some(lifted(20)));
    assert_eq!(step.summary, "var1.225=0->1");
}

#[test]
fn a_key_guard_is_tolerated_on_a_triangle_only() {
    // the same write under the OK button, but nobody stands on a triangle: it is nothing the server sees
    let pressed = entity_script(&[], &looping(&when_ok_pressed(&when_moment(3, 9, &setword(2, 0, 9)))));
    assert_eq!(scan(&field_with_player(|s| s.entity("e", vec![(0, pressed)])), &mesh(5)).point_count(), 0, "entering is not a key press");
    // a line whose script asks for OK first is the player pressing OK on the line: not walking onto it
    let door = vec![
        (0, entity_script(&line((0, 0), (0, 50)), &[])),
        (4, concat(&[when_ok_pressed(&when_moment(3, 9, &setword(2, 0, 9))), ret()])),
    ];
    assert_eq!(scan(&field_with_player(|s| s.entity("door", door)), &mesh(5)).point_count(), 0);
    // a key that is not OK (the menu button, 0x10) is not tolerated on a triangle either
    let menu = ifkeyon(0x10, skip(when_moment(3, 9, &setword(2, 0, 9)).len()));
    let wrong = entity_script(&[], &looping(&when_on_triangle(3, &concat(&[menu, when_moment(3, 9, &setword(2, 0, 9))]))));
    assert_eq!(scan(&field_with_player(|s| s.entity("e", vec![(0, wrong)])), &mesh(5)).point_count(), 0);
}

#[test]
fn a_write_that_only_clears_a_bit_is_no_point() {
    let up = when_on_triangle(8, &when_lift_bit(9, 0, &bitoff(1, 225, 0)));
    let sec = field_with_player(|s| s.entity("ele", vec![(0, entity_script(&[], &looping(&up)))]));
    assert_eq!(scan(&sec, &mesh(9)).point_count(), 0, "the ride back up is not carried out");
    // ...but the clear that comes with a forward step rides along with it
    let both = when_on_triangle(8, &when_moment(3, 20, &concat(&[setword(2, 0, 20), bitoff(1, 225, 0)])));
    let sec = field_with_player(|s| s.entity("ele", vec![(0, entity_script(&[], &looping(&both)))]));
    let story = scan(&sec, &mesh(9));
    assert_eq!(story.point_count(), 1);
    let step = story.advance(lifted(11).encode().as_slice(), 0).unwrap();
    assert_eq!(StoryState::decode(&step.progress), Some(StoryState::at(20)), "the bit is cleared with the move");
    assert_eq!(step.progress, at(20), "and a state with no variable is a version 1 blob again");
}

#[test]
fn a_bit_the_model_does_not_carry_is_no_write_and_no_test() {
    // bit 7 of the reactor byte (the timer's) is not carried: nothing is written and a test of it is not understood
    let timer = when_on_triangle(3, &when_moment(3, 9, &concat(&[biton(1, 225, 7), setword(2, 0, 9)])));
    let sec = field_with_player(|s| s.entity("e", vec![(0, entity_script(&[], &looping(&timer)))]));
    let story = scan(&sec, &mesh(5));
    assert_eq!(story.points()[0].label, "triangle 3: moment <9 -> 9", "only the moment is written");
    let tests_bit_7 = when_on_triangle(3, &when_lift_bit(9, 7, &setword(2, 0, 9)));
    let sec = field_with_player(|s| s.entity("e", vec![(0, entity_script(&[], &looping(&tests_bit_7)))]));
    assert_eq!(scan(&sec, &mesh(5)).point_count(), 0);
    // another variable of the same bank is not carried at all
    let other = when_on_triangle(3, &when_moment(3, 9, &biton(1, 224, 0)));
    let sec = field_with_player(|s| s.entity("e", vec![(0, entity_script(&[], &looping(&other)))]));
    assert_eq!(scan(&sec, &mesh(5)).point_count(), 0);
}

// ---- story points: a line, and entering ----

#[test]
fn a_walk_on_line_that_writes_the_story_is_a_line_point() {
    let line_entity = vec![(0, entity_script(&line((-90, 200), (-50, 200)), &[])), (3, concat(&[when_moment(3, 11, &setword(2, 0, 11)), ret()]))];
    let story = scan(&field_with_player(|s| s.entity("evb", line_entity)), &mesh(3));
    assert_eq!(story.point_count(), 1);
    let point = story.points().remove(0);
    assert_eq!(point.trigger, StoryTrigger::Line(Pos::new(-90.0, 200.0, 7.0), Pos::new(-50.0, 200.0, 7.0)));
    assert_eq!(point.label, "line: moment <11 -> 11");
    // the point is hit by walking over the line, not by standing near it
    assert!(point.is_hit_by(Pos::new(-70.0, 190.0, 0.0), Pos::new(-70.0, 210.0, 0.0)));
    assert!(!point.is_hit_by(Pos::new(-70.0, 190.0, 0.0), Pos::new(-70.0, 199.0, 0.0)));
    assert_eq!(story.advance(&at(10), 0).unwrap().progress, at(11));
    assert_eq!(story.advance(&at(11), 0), None);
    // any walk-on slot counts, `[OK]` (slot 1) does not, and a slot that shares its script with another is one point
    for (slot, count) in [(2usize, 1usize), (4, 1), (5, 1), (6, 1), (1, 0)] {
        let e = vec![(0, entity_script(&line((0, 0), (0, 9)), &[])), (slot, concat(&[when_moment(3, 5, &setword(2, 0, 5)), ret()]))];
        assert_eq!(scan(&field_with_player(|s| s.entity("l", e)), &mesh(3)).point_count(), count, "slot {slot}");
    }
}

#[test]
fn a_line_that_starts_switched_off_or_has_no_literal_line_is_no_point() {
    let body = concat(&[when_moment(3, 11, &setword(2, 0, 11)), ret()]);
    let off = vec![(0, entity_script(&concat(&[line((0, 0), (0, 9)), vec![0xD1, 0]]), &[])), (3, body.clone())];
    assert_eq!(scan(&field_with_player(|s| s.entity("l", off)), &mesh(3)).point_count(), 0, "a script switches it on later");
    let on = vec![(0, entity_script(&concat(&[line((0, 0), (0, 9)), vec![0xD1, 1]]), &[])), (3, body.clone())];
    assert_eq!(scan(&field_with_player(|s| s.entity("l", on)), &mesh(3)).point_count(), 1);
    // not a line entity at all: its slot 3 is nobody's walk-on script
    let plain = vec![(0, entity_script(&[], &[])), (3, body)];
    assert_eq!(scan(&field_with_player(|s| s.entity("n", plain)), &mesh(3)).point_count(), 0);
}

#[test]
fn a_write_only_the_story_guards_is_an_entry_point() {
    // main: IFSW Var[2][0] < 8 -> SETWORD Var[2][0] = 8 (a character walks to a spot and the story moves on)
    let walker = entity_script(&[], &looping(&when_moment(3, 8, &setword(2, 0, 8))));
    let story = scan(&field_with_player(|s| s.entity("walker", vec![(0, walker)])), &mesh(3));
    assert_eq!(story.point_count(), 1);
    let point = story.points().remove(0);
    assert_eq!(point.trigger, StoryTrigger::Enter);
    assert_eq!(point.label, "entry: moment <8 -> 8");
    assert!(!point.is_hit_by(Pos::default(), Pos::new(500.0, 500.0, 0.0)));
    assert_eq!(story.advance(&at(7), 0).unwrap().summary, "moment=7->8");
    assert_eq!(story.advance(&at(8), 0), None);
    // ...its comparison still shapes the phases
    assert_eq!(story.edges(), [8]);
    // the init part runs by itself too
    let init = entity_script(&when_moment(3, 5, &setword(2, 0, 5)), &[]);
    let story = scan(&field_with_player(|s| s.entity("e", vec![(0, init)])), &mesh(3));
    assert_eq!(story.points().iter().map(|p| p.label.as_str()).collect::<Vec<_>>(), ["entry: moment <5 -> 5"]);
}

#[test]
fn a_write_the_server_cannot_be_sure_the_script_reaches_is_not_read() {
    let write = || when_moment(3, 8, &setword(2, 0, 8));
    let points = |main: Vec<u8>| scan(&field_with_player(|s| s.entity("e", vec![(0, entity_script(&[], &main))])), &mesh(3)).point_count();
    assert_eq!(points(looping(&write())), 1, "the plain case");
    // a guard on something the model does not carry (Var[5][3], bank 5)
    assert_eq!(points(looping(&concat(&[ifub_lit(5, 3, 1, 0, skip(write().len())), write()]))), 0);
    // the party (IFPRTYQ) is not carried either
    let party = concat(&[vec![0xCB, 0, skip(write().len())], write()]);
    assert_eq!(points(looping(&party)), 0);
    // the else branch of a test runs when the test FAILS, which the server does not model
    // (the test of a bit the model carries: the else branch of it is the write behind an unconditional jump)
    let else_branch = concat(&[ifub_lit(1, 225, 0, 10, skip(2)), vec![0x10, skip(write().len())], write()]);
    assert_eq!(points(looping(&else_branch)), 0);
    // an early return, a map jump or the end of the game before it
    assert_eq!(points(concat(&[ret(), write()])), 0, "RET first");
    let jump = vec![0x60, 1, 0, 0, 0, 0, 0, 0, 0, 0];
    assert_eq!(points(concat(&[jump, write()])), 0, "MAPJUMP first");
    assert_eq!(points(concat(&[vec![0xFF], write()])), 0, "GAMEOVER first");
    // no condition at all: a script that does not say when it writes is not read
    assert_eq!(points(looping(&setword(2, 0, 8))), 0);
    // an inner loop, or a question for the player, before the write of something that runs by itself
    let waits = concat(&[looping(&ifub_lit(5, 3, 1, 0, 1)), write()]);
    assert_eq!(points(waits), 0, "an entry point does not wait");
    let asks = concat(&[vec![0x48, 0, 0, 0, 0, 0, 5], write()]);
    assert_eq!(points(looping(&asks)), 0, "an entry point does not ask");
    // two tests of the player's triangle make no region
    let twice = when_on_triangle(3, &when_on_triangle(4, &write()));
    assert_eq!(points(looping(&twice)), 0);
}

#[test]
fn points_come_in_a_stable_order_triangles_then_lines_then_entries() {
    let walker = entity_script(&[], &looping(&when_moment(3, 8, &setword(2, 0, 8))));
    let line_entity = vec![(0, entity_script(&line((0, 0), (0, 9)), &[])), (3, concat(&[when_moment(3, 11, &setword(2, 0, 11)), ret()]))];
    let panel = entity_script(&[], &looping(&when_on_triangle(2, &when_moment(3, 9, &setword(2, 0, 9)))));
    let sec = field_with_player(|s| s.entity("walker", vec![(0, walker)]).entity("line", line_entity).entity("panel", vec![(0, panel)]));
    let story = scan(&sec, &mesh(5));
    let points = story.points();
    let triggers: Vec<&str> = points
        .iter()
        .map(|p| match p.trigger {
            StoryTrigger::Region(_) => "region",
            StoryTrigger::Line(..) => "line",
            StoryTrigger::Enter => "entry",
            _ => "?",
        })
        .collect();
    assert_eq!(triggers, ["region", "line", "entry"], "whatever the entity order: the older kinds keep the ids they always had");
    assert_eq!(story.points().iter().map(|p| p.id).collect::<Vec<_>>(), [0, 1, 2]);
}

#[test]
fn writes_that_do_not_nest_are_separate_points_and_nested_ones_are_one() {
    // two triangles' worth of writes on the same triangle with guards that are not one inside the other: two points
    let both = concat(&[when_moment(3, 27, &setword(2, 0, 27)), when_moment(0, 123, &setword(2, 0, 127))]);
    let panel = entity_script(&[], &looping(&when_on_triangle(2, &both)));
    let story = scan(&field_with_player(|s| s.entity("panel", vec![(0, panel)])), &mesh(5));
    assert_eq!(story.points().iter().map(|p| p.label.as_str()).collect::<Vec<_>>(), ["triangle 2: moment <27 -> 27", "triangle 2: moment ==123 -> 127"]);
    // a second write under the same guard joins the first as a second effect of one point
    let same = when_moment(3, 27, &concat(&[setword(2, 0, 27), biton(1, 225, 0)]));
    let panel = entity_script(&[], &looping(&when_on_triangle(2, &same)));
    let story = scan(&field_with_player(|s| s.entity("panel", vec![(0, panel)])), &mesh(5));
    assert_eq!(story.point_count(), 1);
    assert_eq!(story.advance(&at(5), 0).unwrap().summary, "moment=5->27 var1.225=0->1");
}

#[test]
fn a_battle_the_script_starts_is_named_in_the_label_and_not_run() {
    let battle = vec![0x70, 0, 0x2C, 0x01]; // BATTLE 300
    let body = concat(&[setword(2, 0, 6), battle]);
    let panel = entity_script(&[], &looping(&when_on_triangle(3, &when_moment(0, 1, &body))));
    let story = scan(&field_with_player(|s| s.entity("panel", vec![(0, panel)])), &mesh(5));
    let label = story.points().remove(0).label;
    assert!(label.starts_with("triangle 3: moment ==1 -> 6 (the script then starts battle 300"), "{label}");
}

#[test]
fn damaged_scripts_never_panic() {
    let sec = field_with_player(|s| s.entity("ele", vec![(0, the_ride())]));
    let m = mesh(9);
    for cut in 0..sec.len() {
        if let Ok(script) = FieldScript::parse(&sec[..cut]) {
            let _ = AreaStory::scan(&script, &m);
        }
    }
    for at in 0..sec.len() {
        let mut b = sec.clone();
        b[at] ^= 0xFF;
        if let Ok(script) = FieldScript::parse(&b) {
            let _ = AreaStory::scan(&script, &m);
        }
    }
}
