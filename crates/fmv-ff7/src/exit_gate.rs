//! Script exits as the area's exits: the `MAPJUMP`s that a line's trigger script runs when the player walks onto
//! the line, with the conditions they sit under, and how those conditions are judged against an account's story.
//!
//! [`crate::script::FieldScript::script_exits`] describes every `MAPJUMP` of a field (what runs it, the line, the
//! `IF`s around it). [`script_exits`] keeps the ones a walk can set off and that the server can place and
//! judge, and turns them into [`Exit`]s of kind [`ExitKind::Script`]; the guards of each stay with the area
//! ([`ScriptGate`]) and [`ScriptGate::state`] judges them for a progression blob
//! (`AreaRuntime::exit_state`). The rules:
//!
//! * **Which jumps.** Only the walk-on slots of a line (slots 2 to 6, [`crate::script::ExitTrigger::is_walk_on`];
//!   `[OK]` needs the button, a talk or a contact needs the player to do something, a director's jump runs by
//!   itself), on a line that does not start switched off (a script switches it on later, when the server cannot
//!   tell), to a field the archive offers, **with an arrival the data names** (the
//!   `(0, 0)` on triangle 0 of a placeholder says "where the target's own scripts put the player", which the server
//!   cannot run, so such an exit is not offered). Several jumps of one line are several exits on the same line;
//!   at most one of them is open for a given story, and the server takes the first that is.
//! * **Which guards, and what they say.** The guards of an exit are a conjunction (the `IF`s around the jump, the
//!   `else` branch of one negated). A guard is **true** or **false** when it compares the moment, or a tracked
//!   variable ([`crate::story::TRACKED_VARS`]), with a literal; an exit with a false guard is **closed** (the exits
//!   of `elevtr1` that lead to the later story say so with `Var[2][0] < 33`, nothing else keeps them closed). A
//!   guard that reads anything else (a variable the progression does not carry, a key the player has to press, the
//!   party) cannot be judged: the exit stays closed and the server says so once per area. A false guard wins
//!   over one that cannot be judged (the exit is closed whatever the rest says).
//! * **Where it leads.** A script exit of a field a server SERVES (`Ff7Adapter::load_field`, not the survey's loader)
//!   whose target is not one of the [`crate::REVIEWED_FIELDS`] is **closed whatever its guards say**
//!   ([`ScriptGate::lead_out`]): the story decides it, so a party's story moving on could send a player into a field whose
//!   own script exits, points and links are left off (the lift of `elevtr1` leads to `smkin_1` and `smkin_2` from moment 33
//!   on, and a member that comes to it after its leader's finale is judged on that story). Gateways keep their behaviour
//!   (data, open for every story: `nrthmk` to `md8_4` is how a party reaches the end of the reviewed slice on purpose).
//!   The exit stays in the list and stays judged for the survey, so that what the data says is still there to read;
//!   reviewing the target field opens it with no other change.
//! * **No flow analysis.** The guards of a jump reached through `REQ` calls are the callers' and the callee's
//!   together, whether or not an earlier instruction of the script would have changed what they test: a conjunction
//!   that can never hold simply makes an exit that is never open.
//!
//! Measured on the owned FF7 Steam Edition (en), 2026-11-09 (`tests/chain_real.rs` and `examples/exits_dump.rs
//! --all`): see the ignored real-data test `script_exits_of_the_archive_in_numbers`.

use fmv_adapter_api::{Exit, ExitKind, ExitState};
use fmv_core::Pos;

use crate::area_content::field_direction_to_facing;
use crate::script::{CompareOp, CompareWidth, ExitGuard, FieldScript, GuardTest, GuardValue, Maplist, ScriptExit};
use crate::story::StoryState;

/// Most script exits one field offers. A real field has fewer than a dozen; the debug field that has the most
/// (`move_f`) still has fewer than this once placeholders and closed-for-ever exits are left out.
pub(crate) const MAX_SCRIPT_EXITS_PER_AREA: usize = 96;

/// The conditions of one script exit, kept by the area under the id its [`Exit`] carries.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ScriptGate {
    guards: Vec<ExitGuard>,
    /// The exit leads out of the reviewed fields and the area is served: it is closed whatever the guards say (module
    /// docs, "Where it leads").
    leads_out: bool,
}

/// What a guard comes to for a story.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Verdict {
    True,
    False,
    /// The guard reads something the story model does not carry (what, as a short phrase).
    Unknown(String),
}

impl ScriptGate {
    /// Whether the exit is open for the story `progress` (see the module docs). A progression that cannot be read
    /// opens nothing a script decides, and an exit that leads out of the reviewed fields is closed even where its
    /// guards say open (what the guards cannot judge stays unjudged).
    pub(crate) fn state(&self, progress: &[u8]) -> ExitState {
        match self.guards_state(progress) {
            ExitState::Open if self.leads_out => ExitState::Closed,
            other => other,
        }
    }

    /// The exit leads to a field that is not reviewed: for a server that serves the area it is closed (module docs,
    /// "Where it leads").
    pub(crate) fn lead_out(&mut self) {
        self.leads_out = true;
    }

    /// What the guards alone say of `progress`.
    fn guards_state(&self, progress: &[u8]) -> ExitState {
        let Some(story) = StoryState::decode(progress) else {
            return ExitState::Unknown("the progression is not one this adapter reads".to_string());
        };
        let mut unknown: Option<String> = None;
        for guard in &self.guards {
            match verdict(guard, &story) {
                Verdict::True => {}
                Verdict::False => return ExitState::Closed,
                Verdict::Unknown(why) => {
                    unknown.get_or_insert(why);
                }
            }
        }
        unknown.map_or(ExitState::Open, ExitState::Unknown)
    }

    /// The conditions of this exit in words (`Var[2][0] < 33 and ...`; `no conditions` for a jump with none): what a
    /// log says about an exit that is left off.
    pub(crate) fn conditions(&self) -> String {
        if self.guards.is_empty() {
            "no conditions".to_string()
        } else {
            self.guards.iter().map(ToString::to_string).collect::<Vec<_>>().join(" and ")
        }
    }

    /// The guards of this exit (tests read them).
    #[cfg(test)]
    pub(crate) fn guards(&self) -> &[ExitGuard] {
        &self.guards
    }
}

/// The script exits of `script` as the area's exits, and the gates the exits' ids index (see the module docs).
/// `listed_key` maps a maplist name to the key under which the archive offers that field (`None`: it does not).
pub(crate) fn script_exits(script: &FieldScript, maplist: &Maplist, listed_key: impl Fn(&str) -> Option<String>) -> (Vec<Exit>, Vec<ScriptGate>) {
    let mut exits: Vec<Exit> = Vec::new();
    let mut gates: Vec<ScriptGate> = Vec::new();
    for e in script.script_exits(maplist) {
        if exits.len() >= MAX_SCRIPT_EXITS_PER_AREA {
            break;
        }
        if let Some((exit, gate)) = keep(&e, &listed_key) {
            // The same jump reached by two chains with the same conditions is one exit.
            let twin = exits.iter().zip(&gates).any(|(x, g)| x.line == exit.line && x.target == exit.target && x.arrival == exit.arrival && g.guards == gate.guards);
            if twin {
                continue;
            }
            let id = u32::try_from(gates.len()).unwrap_or(u32::MAX);
            exits.push(Exit { kind: ExitKind::Script { id }, ..exit });
            gates.push(gate);
        }
    }
    (exits, gates)
}

/// The exit and gate of `e`, if it is a jump the server offers (module docs, "Which jumps").
fn keep(e: &ScriptExit, listed_key: &impl Fn(&str) -> Option<String>) -> Option<(Exit, ScriptGate)> {
    let line = e.line.filter(|l| l.enabled != Some(false))?;
    if !e.trigger.is_walk_on() || e.arrival_is_placeholder() {
        return None;
    }
    let target = listed_key(e.target.as_deref()?)?;
    let point = |p: [i16; 3]| Pos::new(f32::from(p[0]), f32::from(p[1]), f32::from(p[2]));
    let exit = Exit {
        line: (point(line.a), point(line.b)),
        target,
        arrival: Pos::new(f32::from(e.x), f32::from(e.y), 0.0),
        arrival_facing: Some(field_direction_to_facing(e.direction)),
        arrival_hint: u32::try_from(e.triangle).ok(),
        kind: ExitKind::Gateway, // the caller gives it its id
    };
    Some((exit, ScriptGate { guards: e.guards.clone(), leads_out: false }))
}

/// `guard` for `story`: what an `IF` around the jump comes to (negated for an `else` branch).
fn verdict(guard: &ExitGuard, story: &StoryState) -> Verdict {
    let held = match guard.test {
        GuardTest::Compare { width, left, op, right } => compare(width, left, op, right, story),
        GuardTest::Keys { .. } => Err("a key the player has to press".to_string()),
        GuardTest::InParty { .. } | GuardTest::IsMember { .. } => Err("the party".to_string()),
    };
    match held {
        Ok(test) if test != guard.negated => Verdict::True,
        Ok(_) => Verdict::False,
        Err(why) => Verdict::Unknown(why),
    }
}

/// `left <op> right` for `story`, or what it reads that the model does not carry.
fn compare(width: CompareWidth, left: GuardValue, op: CompareOp, right: GuardValue, story: &StoryState) -> Result<bool, String> {
    if matches!(op, CompareOp::BitOn | CompareOp::BitOff) {
        // `left` has bit `right` set (or clear): the bit is a literal and the model must carry that bit.
        let (GuardValue::Var { bank, address }, GuardValue::Literal(bit)) = (left, right) else {
            return Err("a bit test of something that is not a variable and a literal".to_string());
        };
        let bit = u8::try_from(bit).map_err(|_| "a bit that is not one of a byte or a word".to_string())?;
        // the bits above 7 of a word are the bits of the byte after it
        let (at, bit) = match (width, bit) {
            (_, 0..=7) => (address, bit),
            (CompareWidth::Byte, _) => return Err("a bit that a byte does not have".to_string()),
            (_, 8..=15) => (address.checked_add(1).ok_or_else(|| "a word past the end of its bank".to_string())?, bit - 8),
            _ => return Err("a bit that a word does not have".to_string()),
        };
        let carried = story.known_bit(bank, at, bit).ok_or_else(|| format!("bit {bit} of Var[{bank}][{at}] is not carried"))?;
        return Ok(carried == matches!(op, CompareOp::BitOn));
    }
    let value = |v: GuardValue| -> Result<i64, String> {
        match v {
            GuardValue::Literal(l) => Ok(i64::from(l)),
            GuardValue::Var { bank, address } => {
                let read = match width {
                    CompareWidth::Byte => story.known_byte(bank, address).map(i64::from),
                    CompareWidth::SignedWord => story.known_word(bank, address).map(|w| i64::from(i16::from_le_bytes(w.to_le_bytes()))),
                    CompareWidth::Word => story.known_word(bank, address).map(i64::from),
                };
                read.ok_or_else(|| format!("Var[{bank}][{address}] is not carried"))
            }
        }
    };
    let (a, b) = (value(left)?, value(right)?);
    Ok(match op {
        CompareOp::Eq => a == b,
        CompareOp::Ne => a != b,
        CompareOp::Gt => a > b,
        CompareOp::Lt => a < b,
        CompareOp::Ge => a >= b,
        CompareOp::Le => a <= b,
        CompareOp::And => a & b != 0,
        CompareOp::Xor => a ^ b != 0,
        CompareOp::Or => a | b != 0,
        CompareOp::BitOn | CompareOp::BitOff => return Err("a bit test".to_string()),
        CompareOp::Other(code) => return Err(format!("a comparison the script engine documents no meaning for (code {code})")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::synth::SynthScript;
    use crate::script::MAPLIST_NAME_LEN;

    fn maplist(names: &[&str]) -> Maplist {
        let mut v = (names.len() as u16).to_le_bytes().to_vec();
        for n in names {
            let mut rec = [0u8; MAPLIST_NAME_LEN];
            rec[..n.len()].copy_from_slice(n.as_bytes());
            v.extend_from_slice(&rec);
        }
        Maplist::parse(&v).unwrap()
    }

    // --- a tiny assembler (the opcodes of a line that jumps under conditions) ---

    const RET: u8 = 0x00;

    fn line(x: i16) -> Vec<u8> {
        let mut v = vec![0xD0];
        for c in [x, -60, 5, x, 60, 5] {
            v.extend_from_slice(&c.to_le_bytes());
        }
        v
    }

    fn mapjump(field: u16, x: i16, y: i16, tri: i16, dir: u8) -> Vec<u8> {
        let mut v = vec![0x60];
        v.extend_from_slice(&field.to_le_bytes());
        for c in [x, y, tri] {
            v.extend_from_slice(&c.to_le_bytes());
        }
        v.push(dir);
        v
    }

    /// `IFSW Var[2][0] <op> literal`, skipping `body` bytes when it fails.
    fn if_moment(op: u8, literal: i16, body: usize) -> Vec<u8> {
        let mut v = vec![0x16, 0x20, 0, 0];
        v.extend_from_slice(&literal.to_le_bytes());
        v.push(op);
        v.push(u8::try_from(body + 1).unwrap());
        v
    }

    /// `IFUB Var[bank][addr] <op> literal`, skipping `body` bytes when it fails.
    fn if_byte(bank: u8, addr: u8, op: u8, literal: u8, body: usize) -> Vec<u8> {
        vec![0x14, bank << 4, addr, literal, op, u8::try_from(body + 1).unwrap()]
    }

    fn ifkeyon(keys: u16, body: usize) -> Vec<u8> {
        let k = keys.to_le_bytes();
        vec![0x31, k[0], k[1], u8::try_from(body + 1).unwrap()]
    }

    fn jmpf(skip: usize) -> Vec<u8> {
        vec![0x10, u8::try_from(skip + 1).unwrap()]
    }

    fn cat(parts: &[Vec<u8>]) -> Vec<u8> {
        parts.concat()
    }

    /// The elevator of the first reactor, in miniature: the lift bit decides up or down, the moment the north or
    /// the south reactor.
    fn lift_script() -> Vec<u8> {
        let j = |field, tri| mapjump(field, -50, 40, tri, 64);
        let (a, b, c, d) = (j(1, 60), j(2, 52), j(3, 92), j(4, 92));
        // bit clear: if moment < 33 { a } else { b }; bit set: if moment < 33 { c } else { d }
        let up = cat(&[if_moment(3, 33, a.len() + 2), a.clone(), jmpf(b.len()), b.clone()]);
        let down = cat(&[if_moment(3, 33, c.len() + 2), c.clone(), jmpf(d.len()), d.clone()]);
        let code = cat(&[
            if_byte(1, 225, 10, 0, up.len() + 2),
            up.clone(),
            jmpf(down.len()),
            // the else branch of the bit test: the lift is down
            down,
            vec![RET],
        ]);
        let s = SynthScript::default().entity("hero", vec![(0, vec![RET])]).entity("jp0", vec![(0, cat(&[line(-185), vec![RET]])), (3, code)]);
        s.build()
    }

    fn exits_and_gates(section: &[u8], names: &[&str]) -> (Vec<Exit>, Vec<ScriptGate>) {
        let script = FieldScript::parse(section).unwrap();
        script_exits(&script, &maplist(names), |n| Some(n.to_string()))
    }

    fn lifted(moment: u16, bit: bool) -> Vec<u8> {
        let mut s = StoryState::at(moment);
        if bit {
            s.set_bit(1, 225, 0, true);
        }
        s.encode()
    }

    /// The open exit of the elevator's line for a story, if one is.
    fn open_target(exits: &[Exit], gates: &[ScriptGate], progress: &[u8]) -> Option<String> {
        let open: Vec<&Exit> = exits
            .iter()
            .filter(|e| matches!(e.kind, ExitKind::Script { id } if gates.get(id as usize).is_some_and(|g| g.state(progress) == ExitState::Open)))
            .collect();
        assert!(open.len() <= 1, "at most one exit of the line is open: {open:?}");
        open.first().map(|e| e.target.clone())
    }

    #[test]
    fn a_walk_on_line_with_four_guarded_jumps_gives_four_exits_on_one_line_and_one_is_open_for_each_story() {
        let (exits, gates) = exits_and_gates(&lift_script(), &["zero", "north_up", "south_up", "north_down", "south_down"]);
        assert_eq!(exits.len(), 4, "{exits:#?}");
        assert!(exits.iter().all(|e| e.same_line_as(&exits[0])), "one line");
        assert_eq!(exits[0].line, (Pos::new(-185.0, -60.0, 5.0), Pos::new(-185.0, 60.0, 5.0)));
        assert_eq!(exits.iter().map(|e| e.target.as_str()).collect::<Vec<_>>(), ["north_up", "south_up", "north_down", "south_down"]);
        assert!(exits.iter().enumerate().all(|(i, e)| e.kind == ExitKind::Script { id: i as u32 }));
        assert_eq!((exits[2].arrival, exits[2].arrival_hint), (Pos::new(-50.0, 40.0, 0.0), Some(92)));
        assert!(exits[2].arrival_facing.is_some_and(f32::is_finite));
        // the lift up, and the story before and after the later chapter
        assert_eq!(open_target(&exits, &gates, &[]).as_deref(), Some("north_up"), "a new account");
        assert_eq!(open_target(&exits, &gates, &lifted(12, false)).as_deref(), Some("north_up"));
        assert_eq!(open_target(&exits, &gates, &lifted(32, false)).as_deref(), Some("north_up"));
        assert_eq!(open_target(&exits, &gates, &lifted(33, false)).as_deref(), Some("south_up"), "later story: the other reactor");
        // the lift down (after the ride)
        assert_eq!(open_target(&exits, &gates, &lifted(12, true)).as_deref(), Some("north_down"));
        assert_eq!(open_target(&exits, &gates, &lifted(33, true)).as_deref(), Some("south_down"));
        // the ones that belong to the later story are closed because their guards say so
        assert_eq!(gates[1].state(&lifted(12, false)), ExitState::Closed);
        assert_eq!(gates[3].state(&lifted(12, false)), ExitState::Closed);
        assert_eq!(gates[1].guards().len(), 2);
    }

    #[test]
    fn an_exit_that_leads_out_of_the_reviewed_fields_is_closed_whatever_the_story_says_and_what_cannot_be_judged_stays_unjudged() {
        let (exits, mut gates) = exits_and_gates(&lift_script(), &["zero", "north_up", "south_up", "north_down", "south_down"]);
        // as the survey loads them: from moment 33 the lift leads to the other reactor
        assert_eq!(open_target(&exits, &gates, &lifted(33, false)).as_deref(), Some("south_up"));
        assert_eq!(open_target(&exits, &gates, &lifted(33, true)).as_deref(), Some("south_down"));
        // a served area closes the two that lead out of the reviewed fields
        gates[1].lead_out();
        gates[3].lead_out();
        assert_eq!(open_target(&exits, &gates, &lifted(33, false)), None, "nothing is open for the later story");
        assert_eq!(open_target(&exits, &gates, &lifted(33, true)), None);
        assert_eq!(open_target(&exits, &gates, &lifted(500, false)), None);
        assert_eq!(gates[1].state(&lifted(33, false)), ExitState::Closed, "closed, not unjudged");
        // the ones that stay inside are judged as before
        assert_eq!(open_target(&exits, &gates, &[]).as_deref(), Some("north_up"));
        assert_eq!(open_target(&exits, &gates, &lifted(32, false)).as_deref(), Some("north_up"));
        assert_eq!(open_target(&exits, &gates, &lifted(12, true)).as_deref(), Some("north_down"));
        assert_eq!(open_target(&exits, &gates, &lifted(32, true)).as_deref(), Some("north_down"));
        assert_eq!(open_target(&exits, &gates, &lifted(33, true)), None, "and they are closed by the story from 33 on");
        // a progression nobody can read is still unjudged, not closed
        assert!(matches!(gates[1].state(&[7, 7]), ExitState::Unknown(_)));
    }

    #[test]
    fn a_progression_the_adapter_cannot_read_opens_no_script_exit() {
        let (exits, gates) = exits_and_gates(&lift_script(), &["zero", "a", "b", "c", "d"]);
        for blob in [&[7u8, 7][..], &[3, 1, 0], &[1, 1]] {
            for gate in &gates {
                assert!(matches!(gate.state(blob), ExitState::Unknown(_)), "{blob:?}");
            }
        }
        assert_eq!(exits.len(), 4);
    }

    #[test]
    fn a_guard_the_model_cannot_judge_keeps_the_exit_closed_and_says_what_it_reads() {
        // Var[5][3] == 1 -> jump; the moment is not what the guard reads
        let body = mapjump(1, 0, 0, 3, 0);
        let code = cat(&[if_byte(5, 3, 0, 1, body.len()), body.clone(), vec![RET]]);
        let s = SynthScript::default().entity("jp", vec![(0, cat(&[line(0), vec![RET]])), (2, code)]);
        let (exits, gates) = exits_and_gates(&s.build(), &["zero", "next"]);
        // arrival (0, 0) on triangle 3 is not a placeholder
        assert_eq!(exits.len(), 1);
        let ExitState::Unknown(why) = gates[0].state(&[]) else { panic!("{:?}", gates[0].state(&[])) };
        assert!(why.contains("Var[5][3]"), "{why}");
        // a key press cannot be judged either; the party neither
        let code = cat(&[ifkeyon(0x220, body.len()), body.clone(), vec![RET]]);
        let s = SynthScript::default().entity("jp", vec![(0, cat(&[line(0), vec![RET]])), (4, code)]);
        let (_, gates) = exits_and_gates(&s.build(), &["zero", "next"]);
        assert!(matches!(gates[0].state(&[]), ExitState::Unknown(w) if w.contains("key")));
        let code = cat(&[vec![0xCB, 0, u8::try_from(body.len() + 1).unwrap()], body.clone(), vec![RET]]);
        let s = SynthScript::default().entity("jp", vec![(0, cat(&[line(0), vec![RET]])), (4, code)]);
        let (_, gates) = exits_and_gates(&s.build(), &["zero", "next"]);
        assert!(matches!(gates[0].state(&[]), ExitState::Unknown(w) if w.contains("party")));
        // a false guard wins over one that cannot be judged: this exit is closed for the story, whatever Var[5][3] is
        let inner = cat(&[if_byte(5, 3, 0, 1, body.len()), body.clone()]);
        let code = cat(&[if_moment(3, 5, inner.len()), inner, vec![RET]]);
        let s = SynthScript::default().entity("jp", vec![(0, cat(&[line(0), vec![RET]])), (4, code)]);
        let (_, gates) = exits_and_gates(&s.build(), &["zero", "next"]);
        assert_eq!(gates[0].state(&StoryState::at(9).encode()), ExitState::Closed);
        assert!(matches!(gates[0].state(&StoryState::at(2).encode()), ExitState::Unknown(_)));
    }

    #[test]
    fn what_a_server_cannot_place_or_trust_is_not_offered() {
        let go = |field, x, y, tri| cat(&[mapjump(field, x, y, tri, 0), vec![RET]]);
        let names = ["zero", "one", "two"];
        // not a walk-on slot: [OK] (slot 1) and a model's talk
        let s = SynthScript::default().entity("door", vec![(0, cat(&[line(0), vec![RET]])), (1, go(1, 5, 5, 5))]);
        assert!(exits_and_gates(&s.build(), &names).0.is_empty(), "[OK] needs the button");
        // a placeholder arrival
        let s = SynthScript::default().entity("door", vec![(0, cat(&[line(0), vec![RET]])), (4, go(1, 0, 0, 0))]);
        assert!(exits_and_gates(&s.build(), &names).0.is_empty(), "(0, 0) on triangle 0 says where the target's scripts put the player");
        // a line that starts switched off
        let s = SynthScript::default().entity("door", vec![(0, cat(&[line(0), vec![0xD1, 0], vec![RET]])), (4, go(1, 5, 5, 5))]);
        assert!(exits_and_gates(&s.build(), &names).0.is_empty());
        let s = SynthScript::default().entity("door", vec![(0, cat(&[line(0), vec![0xD1, 1], vec![RET]])), (4, go(1, 5, 5, 5))]);
        assert_eq!(exits_and_gates(&s.build(), &names).0.len(), 1, "switched on");
        // a target past the maplist, and one the archive does not offer
        let s = SynthScript::default().entity("door", vec![(0, cat(&[line(0), vec![RET]])), (4, go(9, 5, 5, 5)), (5, go(2, 5, 5, 5))]);
        let section = s.build();
        let script = FieldScript::parse(&section).unwrap();
        let (exits, _) = script_exits(&script, &maplist(&names), |n| (n != "two").then(|| n.to_string()));
        assert!(exits.is_empty(), "{exits:?}: id 9 is past the maplist and `two` is not listed");
        // the same jump twice (two slots, one script) is one exit
        let one = go(1, 5, 5, 5);
        let s = SynthScript::default().entity("door", vec![(0, cat(&[line(0), vec![RET]])), (4, one.clone()), (5, one)]);
        assert_eq!(exits_and_gates(&s.build(), &names).0.len(), 1);
    }

    #[test]
    fn a_field_without_a_line_has_no_exits_and_an_else_branch_is_the_test_failing() {
        let s = SynthScript::default().entity("a", vec![(0, vec![RET])]);
        assert!(exits_and_gates(&s.build(), &[]).0.is_empty());
        let compare = GuardTest::Compare {
            width: CompareWidth::Byte,
            left: GuardValue::Var { bank: 1, address: 225 },
            op: CompareOp::BitOff,
            right: GuardValue::Literal(0),
        };
        let g = |negated| ExitGuard { offset: 0, test: compare, negated };
        let state = |progress: &[u8]| ScriptGate { guards: vec![g(false)], leads_out: false }.state(progress);
        assert_eq!(state(&[]), ExitState::Open, "a new game: the lift is up");
        assert_eq!(state(&lifted(12, true)), ExitState::Closed);
        assert_eq!(ScriptGate { guards: vec![g(true)], leads_out: false }.state(&lifted(12, true)), ExitState::Open, "an else branch is the test failing");
    }

    #[test]
    fn every_comparison_is_judged_as_the_script_engine_would() {
        let var = |bank, address| GuardValue::Var { bank, address };
        let lit = GuardValue::Literal;
        let judge = |width, left, op, right, progress: &[u8]| -> Verdict {
            verdict(&ExitGuard { offset: 0, test: GuardTest::Compare { width, left, op, right }, negated: false }, &StoryState::decode(progress).unwrap())
        };
        let m = StoryState::at(0x0102).encode();
        // the moment, as a byte, a signed word and an unsigned word
        assert_eq!(judge(CompareWidth::Byte, var(2, 0), CompareOp::Eq, lit(2), &m), Verdict::True);
        assert_eq!(judge(CompareWidth::SignedWord, var(2, 0), CompareOp::Eq, lit(0x0102), &m), Verdict::True);
        assert_eq!(judge(CompareWidth::Word, var(2, 0), CompareOp::Gt, lit(0x0101), &m), Verdict::True);
        assert_eq!(judge(CompareWidth::Word, var(2, 0), CompareOp::Le, lit(0x0101), &m), Verdict::False);
        assert_eq!(judge(CompareWidth::Word, lit(200), CompareOp::Lt, var(2, 0), &m), Verdict::True, "the variable may be on the right");
        assert_eq!(judge(CompareWidth::Word, var(2, 0), CompareOp::Ne, lit(0x0102), &m), Verdict::False);
        assert_eq!(judge(CompareWidth::Word, var(2, 0), CompareOp::Ge, lit(0x0102), &m), Verdict::True);
        // a signed word reads a moment above 32767 as negative, as the engine's IFSW does
        let high = StoryState::at(40000).encode();
        assert_eq!(judge(CompareWidth::SignedWord, var(2, 0), CompareOp::Lt, lit(0), &high), Verdict::True);
        assert_eq!(judge(CompareWidth::Word, var(2, 0), CompareOp::Lt, lit(0), &high), Verdict::False);
        // the bit tests, on the moment (a word, bits above 7 are the second byte) and on the tracked bit
        assert_eq!(judge(CompareWidth::Word, var(2, 0), CompareOp::BitOn, lit(1), &m), Verdict::True);
        assert_eq!(judge(CompareWidth::Word, var(2, 0), CompareOp::BitOn, lit(8), &m), Verdict::True, "0x0102 has bit 1 and bit 8 (its second byte's bit 0)");
        assert_eq!(judge(CompareWidth::Word, var(2, 0), CompareOp::BitOn, lit(9), &m), Verdict::False);
        assert_eq!(judge(CompareWidth::Word, var(2, 0), CompareOp::BitOff, lit(9), &m), Verdict::True);
        assert!(matches!(judge(CompareWidth::Byte, var(2, 0), CompareOp::BitOn, lit(9), &m), Verdict::Unknown(_)), "a byte has no bit 9");
        assert!(matches!(judge(CompareWidth::Byte, var(1, 225), CompareOp::BitOn, lit(6), &m), Verdict::Unknown(_)), "bit 6 of the reactor byte is not carried");
        // (bit 7, the countdown's timeout flag, is carried and nothing sets it: it is clear for every story)
        assert_eq!(judge(CompareWidth::Byte, var(1, 225), CompareOp::BitOn, lit(7), &m), Verdict::False);
        assert_eq!(judge(CompareWidth::Byte, var(1, 225), CompareOp::BitOff, lit(7), &m), Verdict::True);
        assert_eq!(judge(CompareWidth::Byte, var(1, 225), CompareOp::BitOn, lit(0), &m), Verdict::False);
        // the arithmetic ones
        assert_eq!(judge(CompareWidth::Byte, var(2, 0), CompareOp::And, lit(2), &m), Verdict::True);
        assert_eq!(judge(CompareWidth::Byte, var(2, 0), CompareOp::And, lit(1), &m), Verdict::False);
        assert_eq!(judge(CompareWidth::Byte, var(2, 0), CompareOp::Xor, lit(2), &m), Verdict::False);
        assert_eq!(judge(CompareWidth::Byte, var(2, 0), CompareOp::Or, lit(0), &m), Verdict::True);
        // what the model does not carry, and the comparisons the engine documents no meaning for
        assert!(matches!(judge(CompareWidth::Byte, var(6, 4), CompareOp::Eq, lit(0), &m), Verdict::Unknown(w) if w.contains("Var[6][4]")));
        assert!(matches!(judge(CompareWidth::Byte, var(1, 225), CompareOp::Eq, lit(0), &m), Verdict::Unknown(_)), "a byte it carries one bit of is not known whole");
        assert!(matches!(judge(CompareWidth::Byte, var(2, 0), CompareOp::Other(11), lit(0), &m), Verdict::Unknown(w) if w.contains("11")));
        assert!(matches!(judge(CompareWidth::Byte, lit(1), CompareOp::BitOn, var(2, 0), &m), Verdict::Unknown(_)));
    }
}
