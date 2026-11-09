//! Checks of the scene interpreter against the owner's REAL FF7 data (FF7 Steam Edition, English; verified 2026-11-09).
//!
//! Every test is `#[ignore]` and also returns early when `FMV_FF7_DATA` is unset, so a normal `cargo test` never touches
//! game data. Run them with
//!
//! ```text
//! FMV_FF7_DATA=<.../data> cargo test -p fmv-ff7 --test scene_real -- --ignored --nocapture
//! ```
//!
//! The files are only ever opened read-only. The assertions record ids, counts, entities, moments and the order of
//! windows (their dialog indexes and speakers); no game text is stored in this repository and none is printed.
//!
//! One test per acceptance point of the proof scene (Cloud and Tifa at the bar of 7th Heaven, field `mds7pb_1`):
//!
//! 1. the trigger is found: [`the_trigger_is_what_the_field_does`], [`the_trigger_npc_is_tifa_where_her_stage_script_puts_her`];
//! 2. the lines come out in order: [`the_scene_plays_its_real_lines_in_order_with_their_speakers`];
//! 3. the witness stands on the floor, clear of everyone: [`the_witness_stands_on_the_floor_clear_of_everyone_the_scene_ever_places`];
//! 4. the interpreter finishes within budget and every op it meets is handled:
//!    [`the_scene_finishes_within_budget_and_every_op_it_meets_is_handled_or_known_to_change_nothing`];
//! 5. what the story carries: [`the_proof_scene_writes_only_its_mark_once_and_a_scene_that_moves_the_moment_reports_it`].

mod script_common;

use fmv_ff7::field::FieldFile;
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::script::dialogue::{End, MessageOrigin};
use fmv_ff7::script::scene::{
    proof, proof_scene, run_to_end, trigger_npc, CastMember, Ground, Look, MoveHow, SceneDef, ScenePlay, ScriptRef, Stage, Stats, Step, Then,
    Trigger, PROOF_FIELD,
};
use fmv_ff7::script::FieldScript;
use fmv_ff7::story::StoryState;
use script_common::{data_dir, flevel_path, load_field};

fn field(name: &str) -> Option<FieldFile> {
    let Some(dir) = data_dir() else {
        eprintln!("FMV_FF7_DATA is not set; skipping");
        return None;
    };
    let mut lgp = LgpArchive::open_path(flevel_path(&dir)).unwrap();
    Some(load_field(&mut lgp, name).unwrap())
}

/// One window of a played scene: who opened it, who speaks, which dialog, whether it asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Line {
    opener: u8,
    speaker: Option<u8>,
    dialog: usize,
    asks: bool,
}

const fn line(opener: u8, speaker: u8, dialog: usize, asks: bool) -> Line {
    Line { opener, speaker: Some(speaker), dialog, asks }
}

struct Played {
    lines: Vec<Line>,
    end: End,
    stats: Stats,
    /// Every stage event, the initial placements first.
    stage: Vec<Stage>,
}

/// Play the proof scene at `moment`, answering each question from `answers`.
fn play(f: &FieldFile, moment: u16, answers: &[usize]) -> Played {
    let def = proof_scene();
    let mut p = ScenePlay::new(f, &def, &StoryState::at(moment), 1).unwrap();
    let mut answers = answers.iter().copied();
    let mut given = None;
    let (mut lines, mut stage) = (Vec::new(), Vec::new());
    stage.extend(p.initial_stage_events().iter().cloned());
    let end = loop {
        let beat = p.next(given.take());
        stage.extend(beat.stage.iter().map(|t| t.what.clone()));
        match beat.then {
            Then::End { how, .. } => break how,
            Then::Window(w) => {
                let MessageOrigin::Canon { dialog, .. } = w.message.origin else { panic!("an added line in a canon scene") };
                lines.push(Line { opener: w.opener, speaker: w.speaker, dialog, asks: w.message.choice.is_some() });
                if w.message.choice.is_some() {
                    given = answers.next();
                }
            }
        }
        assert!(lines.len() < 64, "a scene that never ends");
    };
    Played { lines, end, stats: p.stats().clone(), stage }
}

const CLOUD: u8 = proof::CLOUD;
const TIFA: u8 = proof::TIFA;

// ---- 1. the trigger is found -----------------------------------------------------------------

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_trigger_is_what_the_field_does() {
    let Some(f) = field(PROOF_FIELD) else { return };
    let script = FieldScript::from_field(&f).unwrap();
    let def = proof_scene();

    // The entities the data names are the entities the field has.
    let names: Vec<_> = [proof::ID, CLOUD, TIFA].iter().map(|e| script.entity_name(usize::from(*e)).unwrap().to_string()).collect();
    assert_eq!(names, ["ID", "cloud", "tifa"]);
    assert_eq!(def.trigger.name, "tifa");
    assert_eq!(def.trigger.entity, TIFA);
    // Cloud and Tifa are party members' entities (`PC`), which is why the server never announces them as NPCs.
    let players = script.player_entities();
    assert!(players.contains(&(usize::from(CLOUD), 0)) && players.contains(&(usize::from(TIFA), 2)), "{players:?}");

    // The `ID` entity's script 0 starts the scene: the three steps, one after the other, are the scene's root.
    let listing = script.disassemble(usize::from(proof::ID), 0).unwrap();
    let steps: Vec<&str> = listing.iter().map(|i| i.mnemonic).collect();
    let at = (0..listing.len().saturating_sub(2))
        .find(|&i| {
            let (a, b, c) = (&listing[i], &listing[i + 1], &listing[i + 2]);
            a.mnemonic == "BITON"
                && a.operand("var").is_some_and(|v| (v.bank, v.value) == (3, 212))
                && a.literal("bit") == Some(2)
                && b.mnemonic == "REQEW"
                && (b.literal("entity"), b.literal("script")) == (Some(i32::from(CLOUD)), Some(i32::from(proof::CLOUD_SITS)))
                && c.mnemonic == "REQSW"
                && (c.literal("entity"), c.literal("script")) == (Some(i32::from(TIFA)), Some(i32::from(proof::COUNTER_TALK)))
        })
        .unwrap_or_else(|| panic!("the field's own trigger is not what the scene says it is: {steps:?}"));
    assert!(at > 0);
    assert_eq!(
        def.root,
        [
            Step::SetBit { bank: 3, address: 212, bit: 2 },
            Step::Await(ScriptRef::new(CLOUD, proof::CLOUD_SITS)),
            Step::Start(ScriptRef::new(TIFA, proof::COUNTER_TALK)),
        ]
    );
    // It watches Cloud's triangle: 62, the stool, which is where the cast puts Cloud.
    assert!(
        listing.iter().any(|i| i.mnemonic == "SETWORD" && i.operand("var").is_some_and(|v| (v.bank, v.value) == (6, 29)) && i.literal("value") == Some(62)),
        "the loop's mark is triangle 62"
    );
    assert_eq!(def.member(CLOUD).and_then(|m| m.start).map(|s| s.triangle), Some(62));
    let cloud_sits = script.disassemble(usize::from(CLOUD), usize::from(proof::CLOUD_SITS + 1)).unwrap();
    assert!(
        cloud_sits.iter().any(|i| i.mnemonic == "XYZI" && i.literal("triangle") == Some(62) && i.literal("x") == Some(-95) && i.literal("y") == Some(-144)),
        "Cloud's script 10 sits him at the stool the cast starts him at"
    );

    // Tifa's talk script is gated on the moment the scene is offered at, and her stage script sets the talk radius.
    let talk = script.disassemble(usize::from(TIFA), usize::from(proof::TALK)).unwrap();
    assert!(
        talk.iter().any(|i| i.mnemonic == "IFSW" && i.operand("left").is_some_and(|l| (l.bank, l.value) == (2, 0)) && i.literal("right") == Some(72) && i.literal("op") == Some(0)),
        "Tifa's talk script tests Var[2][0] == 72"
    );
    assert!(def.applies_at(72));
    let stage = script.disassemble(usize::from(TIFA), usize::from(proof::TIFA_STAGE)).unwrap();
    assert!(
        stage.iter().any(|i| i.mnemonic == "TALKR" && i.literal("radius") == Some(i32::from(def.trigger.talk_radius))),
        "her stage script sets the radius the data records"
    );
    assert!(script.span(usize::from(TIFA), usize::from(def.trigger.slot)).is_some());
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_trigger_npc_is_tifa_where_her_stage_script_puts_her() {
    let Some(f) = field(PROOF_FIELD) else { return };
    let def = proof_scene();
    let npc = trigger_npc(&f, &def, &StoryState::at(72)).expect("the trigger entity is found in the field");
    assert_eq!((npc.entity, npc.name.as_str()), (TIFA, "tifa"));
    // Tifa's model and standing clip are the playable table's own (`ff7/tifa`): the NPC and the cast actor match.
    let tifa = fmv_ff7::resolve_appearance("ff7/tifa").unwrap();
    assert_eq!((npc.model.as_str(), npc.idle.as_deref()), (tifa.hrc, Some(tifa.idle_anim)));
    let cloud = fmv_ff7::resolve_appearance("ff7/cloud").unwrap();
    assert_eq!(cloud.hrc, "aaaa");
    // Behind the counter, in her lane, on triangle 7, facing +x; she can be talked to from 150 units.
    assert_eq!((npc.x, npc.y), (-268, -105));
    let ground = Ground::new(f.walkmesh().unwrap());
    assert_eq!(ground.triangle_at(npc.x, npc.y), Some(7));
    assert!(npc.z.abs() < 1.0, "{npc:?}");
    assert!((npc.heading - 0.0).abs() < 1e-5);
    assert!((npc.talk_radius - 150.0).abs() < 1e-3);
    // Before the scene's moment her stage script puts her somewhere else or nowhere the scene knows: the scene is only
    // offered at 72.
    assert!(!def.applies_at(69) && !def.applies_at(78));
}

// ---- 2. the canon lines play in order ----------------------------------------------------------

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_scene_plays_its_real_lines_in_order_with_their_speakers() {
    let Some(f) = field(PROOF_FIELD) else { return };
    // Answering the drink "something hard" (line 2): eleven windows, Tifa's and Cloud's, the question among them.
    let hard = play(&f, 72, &[2]);
    assert_eq!(
        hard.lines,
        [
            line(TIFA, TIFA, 28, false),
            line(TIFA, TIFA, 29, true),
            line(TIFA, TIFA, 31, false),
            line(TIFA, TIFA, 32, false),
            line(TIFA, CLOUD, 33, false),
            line(TIFA, TIFA, 34, false),
            line(TIFA, TIFA, 35, false),
            line(TIFA, CLOUD, 36, false),
            line(TIFA, TIFA, 37, false),
            line(CLOUD, CLOUD, 38, false),
            line(TIFA, TIFA, 39, false),
        ]
    );
    assert_eq!(hard.end, End::Finished);
    // Cloud's lines are Cloud's: the windows his name is on, whichever script opened them.
    let by = |lines: &[Line], who: u8| lines.iter().filter(|l| l.speaker == Some(who)).count();
    assert_eq!((by(&hard.lines, TIFA), by(&hard.lines, CLOUD)), (8, 3));
    assert_eq!(hard.lines.iter().filter(|l| l.asks).count(), 1);
    // Every window has a speaker: nothing is narration here.
    assert!(hard.lines.iter().all(|l| l.speaker.is_some()));

    // Answering "I don't feel like it" (line 1), or nothing at all (which is the cursor's line, 1): six windows.
    let short = [line(TIFA, TIFA, 28, false), line(TIFA, TIFA, 29, true), line(TIFA, TIFA, 30, false), line(TIFA, TIFA, 37, false), line(CLOUD, CLOUD, 38, false), line(TIFA, TIFA, 39, false)];
    assert_eq!(play(&f, 72, &[1]).lines, short);
    assert_eq!(play(&f, 72, &[]).lines, short);

    // The same story and seed play the same scene (every client and the server agree).
    assert_eq!(play(&f, 72, &[2]).lines, hard.lines);
    // An answer that is no choice is taken as the nearest one (the engine's own clamp), never as a new branch.
    assert_eq!(play(&f, 72, &[99]).lines, hard.lines);
}

// ---- 3. the witness stands on the floor, clear of everyone ------------------------------------

/// Every place the cast takes in a played scene: the initial placements and every walk, sampled each ten units.
fn cast_path(stage: &[Stage]) -> Vec<(u8, f32, f32)> {
    let mut at: std::collections::BTreeMap<u8, (f32, f32)> = Default::default();
    let mut out = Vec::new();
    for s in stage {
        match *s {
            Stage::Place { entity, x, y, .. } => {
                at.insert(entity, (f32::from(x), f32::from(y)));
                out.push((entity, f32::from(x), f32::from(y)));
            }
            Stage::Move { entity, from, x, y, how, .. } => {
                let start = from.map(|(fx, fy)| (f32::from(fx), f32::from(fy))).or_else(|| at.get(&entity).copied());
                let end = (f32::from(x), f32::from(y));
                if let (Some(a), MoveHow::Walk | MoveHow::Jump | MoveHow::Ladder) = (start, how) {
                    let n = ((end.0 - a.0).hypot(end.1 - a.1) / 10.0).ceil().max(1.0) as u32;
                    for k in 0..=n {
                        let t = k as f32 / n as f32;
                        out.push((entity, a.0 + (end.0 - a.0) * t, a.1 + (end.1 - a.1) * t));
                    }
                }
                at.insert(entity, end);
                out.push((entity, end.0, end.1));
            }
            _ => {}
        }
    }
    out
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_witness_stands_on_the_floor_clear_of_everyone_the_scene_ever_places() {
    let Some(f) = field(PROOF_FIELD) else { return };
    let def = proof_scene();
    let ground = Ground::new(f.walkmesh().unwrap());
    let script = FieldScript::from_field(&f).unwrap();
    // One model unit is `default_model_scale(512)` walkmesh units; a ring under a character is 5.5 model units across
    // its radius, so two characters clear of each other stand more than two ring radii apart.
    let scale = fmv_ff7::default_model_scale(script.header().scale);
    let clearance = 2.0 * 5.5 * scale;
    assert!(clearance >= 40.0, "{clearance}");
    let required = clearance + 16.0;

    // Every place the cast ever stands or walks, over every way the scene can go (both answers).
    let mut path: Vec<(u8, f32, f32)> = Vec::new();
    for answers in [vec![2usize], vec![1usize]] {
        path.extend(cast_path(&play(&f, 72, &answers).stage));
    }
    assert!(path.iter().any(|p| p.0 == CLOUD) && path.iter().any(|p| p.0 == TIFA));
    // The NPCs the field announces stand there too.
    let statics: Vec<(f32, f32)> = fmv_ff7::script::dialogue::characters(&script)
        .iter()
        .flat_map(|c| c.places.iter().map(|&(x, y)| (f32::from(x), f32::from(y))))
        .collect();

    // Where the debug preview puts a player: on the floor (the triangle the data names holds it) and within the trigger's reach
    // of Tifa, who stands where her stage script puts her.
    let a = def.arrival;
    assert_eq!(ground.triangle_at(a.x, a.y), Some(a.triangle), "the arrival is on triangle {:?}", ground.triangle_at(a.x, a.y));
    let npc = trigger_npc(&f, &def, &StoryState::at(72)).unwrap();
    let reach = (f32::from(a.x) - f32::from(npc.x)).hypot(f32::from(a.y) - f32::from(npc.y));
    assert!(reach <= npc.talk_radius, "the arrival is {reach:.0} from Tifa (her reach is {})", npc.talk_radius);
    // (the band of floor within Tifa's reach is narrow: at the counter's edge a body's ring, 22, just touches the stools)
    let arrival_gap = scenery_gap(ground.mesh(), f32::from(a.x), f32::from(a.y));
    assert!(arrival_gap >= 5.5 * scale - 2.0, "the arrival is {arrival_gap:.1} from the nearest wall, stool or table");

    assert_eq!(def.witness.len(), 3, "the owner and two watchers");
    for (i, spot) in def.witness.iter().enumerate() {
        // on the floor: inside the triangle the data names, at that triangle's height
        let tri = ground.mesh().triangles.get(usize::from(spot.triangle)).expect("a triangle of the mesh");
        assert!(fmv_ff7::script::triangle_contains_xy(tri, i32::from(spot.x), i32::from(spot.y)), "witness {i} is on triangle {}", spot.triangle);
        let h = ground.height(spot.x, spot.y, Some(spot.triangle), None).expect("a height");
        let want = fmv_ff7::script::triangle_height_at(tri, i32::from(spot.x), i32::from(spot.y)).unwrap() as f32;
        assert!((h - want).abs() < 0.01, "witness {i}: {h} vs {want}");
        // clear of every actor, at every place of the scene
        let nearest = path.iter().map(|&(_, x, y)| (x - f32::from(spot.x)).hypot(y - f32::from(spot.y))).fold(f32::INFINITY, f32::min);
        assert!(nearest >= required, "witness {i} comes within {nearest:.0} of an actor (needs {required:.0})");
        // clear of the NPCs, and of each other
        for &(sx, sy) in &statics {
            assert!((sx - f32::from(spot.x)).hypot(sy - f32::from(spot.y)) >= required, "witness {i} stands on an NPC");
        }
        for (j, other) in def.witness.iter().enumerate().filter(|(j, _)| *j != i) {
            let d = (f32::from(other.x) - f32::from(spot.x)).hypot(f32::from(other.y) - f32::from(spot.y));
            assert!(d >= required, "witnesses {i} and {j} are {d:.0} apart");
        }
        // clear of the scenery the walkmesh knows: the stools and tables are holes in it, so the body's ring (5.5 model units) must not
        // reach a boundary edge (an edge with no neighbour)
        let edge_gap = scenery_gap(ground.mesh(), f32::from(spot.x), f32::from(spot.y));
        assert!(edge_gap >= 5.5 * scale * 1.5, "witness {i} stands {edge_gap:.0} from the nearest wall, stool or table (its ring is {:.0})", 5.5 * scale);
        eprintln!(
            "witness {i}: ({}, {}) triangle {} clear of every actor by {nearest:.0} (needs {required:.0}), {edge_gap:.0} from the nearest wall, stool or table",
            spot.x, spot.y, spot.triangle
        );
    }
}

/// The distance in x and y from `(x, y)` to the nearest boundary edge of the walkmesh (an edge with no neighbour: a wall, or the
/// edge of a hole such as a stool or a table).
fn scenery_gap(mesh: &fmv_ff7::field::Walkmesh, x: f32, y: f32) -> f32 {
    let mut best = f32::INFINITY;
    for (i, tri) in mesh.triangles.iter().enumerate() {
        for e in 0..3 {
            if mesh.neighbor(i, e).is_some() {
                continue;
            }
            let (a, b) = (tri[e], tri[(e + 1) % 3]);
            let (ax, ay, bx, by) = (f32::from(a.x), f32::from(a.y), f32::from(b.x), f32::from(b.y));
            let (dx, dy) = (bx - ax, by - ay);
            let len2 = dx * dx + dy * dy;
            let t = if len2 > 0.0 { (((x - ax) * dx + (y - ay) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
            best = best.min((x - (ax + dx * t)).hypot(y - (ay + dy * t)));
        }
    }
    best
}

// ---- 4. within budget, every op handled --------------------------------------------------------

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_scene_finishes_within_budget_and_every_op_it_meets_is_handled_or_known_to_change_nothing() {
    let Some(f) = field(PROOF_FIELD) else { return };
    // The ops the VM carries out, and the ones it knows change nothing a scene shows.
    let handled = [
        "ANIM!2", "ANIME1", "ANIME2", "ANIMW", "ASK", "BITON", "CANM!2", "DFANM", "DIR", "DIRA", "IFUB", "IFUBL", "JMPF", "JMPFL", "MESSAGE", "MOVE", "PLUS!",
        "REQEW", "REQSW", "RET", "SCR2D", "SCR2DC", "SCRCC", "SCRLW", "SETBYTE", "TURA", "WAIT", "WINDOW", "XYZI",
    ];
    let ignored = ["MENU2", "SOUND", "TALKR", "UC", "WREST"];
    let mut seen_handled = std::collections::BTreeSet::new();
    for answers in [vec![2usize], vec![1usize], vec![]] {
        let p = play(&f, 72, &answers);
        assert_eq!(p.end, End::Finished, "{answers:?}");
        let s = &p.stats;
        assert!(s.steps < 1_000, "{answers:?}: {} instructions (the budget is 200 000)", s.steps);
        assert!(s.unknown.is_empty(), "{answers:?}: opcodes the VM does not know: {:?}", s.unknown);
        for op in s.handled.keys() {
            assert!(handled.contains(op), "{answers:?}: {op} is handled but not listed");
            seen_handled.insert(*op);
        }
        for op in s.ignored.keys() {
            assert!(ignored.contains(op), "{answers:?}: {op} is skipped but not listed");
        }
        assert_eq!((s.dropped_requests, s.unresolved), (0, 0), "{answers:?}: every request was made and every read answered: {s:?}");
        assert_eq!(s.parked, 0, "{answers:?}: nothing needed parking");
    }
    // The longest way through uses every handled op listed above.
    let missing: Vec<_> = handled.iter().filter(|op| !seen_handled.contains(*op)).collect();
    assert!(missing.is_empty(), "listed but never met: {missing:?}");
    // The stage scripts that place the cast run quietly and meet nothing unknown either.
    let def = proof_scene();
    let p = ScenePlay::new(&f, &def, &StoryState::at(72), 1).unwrap();
    assert!(p.setup_stats().unknown.is_empty(), "{:?}", p.setup_stats().unknown);
    assert!(p.setup_stats().steps < 1_000);
    // Everything about the scene is on the cast: the stage events of entities outside it (the camera, the lights)
    // are counted, not shown.
    let p = play(&f, 72, &[2]);
    assert!(p.stage.iter().any(|s| matches!(s, Stage::Camera(_))), "the camera moves are carried");
}

// ---- 5. what the story carries ------------------------------------------------------------------

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_proof_scene_writes_only_its_mark_once_and_a_scene_that_moves_the_moment_reports_it() {
    let Some(f) = field(PROOF_FIELD) else { return };
    let def = proof_scene();
    // The proof scene's script writes the field's own flags (and Tifa's affection points). Of those the story model carries
    // exactly one, the bar's mark (`Var[3][212]` bit 2, `story::BAR_CONVERSATION`): the scene turns it on first, and with it
    // on the story has had the conversation, so the scene is not offered again (acceptance 5). The conversation does not
    // move the story's moment, and the rest is the field's own, said so and not carried.
    let story = StoryState::at(72);
    for answers in [&[2usize][..], &[1][..]] {
        let out = run_to_end(&f, &def, &story, 1, answers).unwrap();
        assert_eq!(out.end, End::Finished);
        assert!(!out.writes.is_story_neutral(), "{:?}", out.writes);
        assert_eq!(out.writes.moment, None, "the conversation does not move the moment");
        let [((3, 212), byte)] = out.writes.tracked[..] else { panic!("exactly the bar's byte is carried: {:?}", out.writes) };
        assert!(byte & 0b100 != 0, "the mark is on: {byte:#b}");
        // (the long way, 'something hard', gives Tifa's affection points a bump: the field's own, never carried)
        assert_eq!(out.writes.untracked.get(&(1, 4)), (answers == [2]).then_some(&5), "Tifa's affection points are the field's own");
        assert!(!out.writes.untracked.contains_key(&(3, 212)), "the bar's byte is carried now");
        // applied once: the story keeps the mark (and only the mark) and the scene is no longer offered to it
        let mut state = story.clone();
        assert!(def.offered_to(&state));
        assert!(out.writes.apply_to(&mut state));
        assert_eq!(state.var(3, 212), 0b100, "only the mark is kept");
        assert!(!out.writes.apply_to(&mut state), "applied twice it changes nothing");
        assert!(def.begun_in(&state) && !def.offered_to(&state), "a story that has had the conversation is not offered it");
        assert_eq!(StoryState::decode(&state.encode()), Some(state), "and the mark is what the stored blob says");
    }

    // A scene whose script DOES write the moment: Tifa's script 15 (Cloud walks out of the bar, Tifa stops him), which
    // sets the moment 81 -> 84 and then jumps to another field (not performed: the scene ends there).
    let leaving = SceneDef {
        id: "ff7/mds7pb_1/tifa-promise".to_string(),
        game: "ff7".to_string(),
        field: PROOF_FIELD.to_string(),
        trigger: Trigger { entity: TIFA, name: "tifa".to_string(), slot: 1, talk_radius: 70, moments: 81..=81 },
        setup: Vec::new(),
        // Flag 0 of `Var[3][213]` is what turns her script's short call from the door into the long goodbye.
        given: vec![fmv_ff7::script::scene::Given { bank: 3, address: 213, value: 0b0000_0001 }],
        root: vec![Step::Start(ScriptRef::new(TIFA, 15))],
        cast: vec![
            CastMember { entity: CLOUD, name: "cloud".into(), look: Look::Appearance("ff7/cloud".into()), start: None },
            CastMember { entity: TIFA, name: "tifa".into(), look: Look::Appearance("ff7/tifa".into()), start: None },
        ],
        speakers: vec![("Cloud".into(), CLOUD), ("Tifa".into(), TIFA)],
        witness: Vec::new(),
        arrival: fmv_ff7::script::scene::Spot { x: 0, y: 0, triangle: 0, direction: 0 },
    };
    let out = run_to_end(&f, &leaving, &StoryState::at(81), 1, &[]).unwrap();
    assert_eq!(out.end, End::MapJump { field: 294 }, "the scene ends where the script leaves the field");
    assert_eq!(out.writes.moment, Some(84));
    assert!(out.stats.unknown.is_empty() || out.stats.steps < 10_000, "{:?}", out.stats.unknown);
    // applied once: the moment goes to 84, and applying it again (or to a later story) changes nothing
    let mut state = StoryState::at(81);
    assert!(out.writes.apply_to(&mut state));
    assert_eq!(state.moment, 84);
    assert!(!out.writes.apply_to(&mut state));
    let mut later = StoryState::at(96);
    assert!(!out.writes.apply_to(&mut later) && later.moment == 96, "a story never goes back");
    eprintln!("the leaving scene: {} windows, ends {:?}, writes {:?}, unknown {:?}", out.windows, out.end, out.writes.moment, out.stats.unknown);
}

// ---- robustness: every script of the game ------------------------------------------------------

/// Run the VM on EVERY script slot of EVERY field of the game as if it were a scene's root: none may panic or run past its
/// budget, and what the VM does not know is counted by mnemonic (the survey of what a later scene would need).
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_vm_ends_every_script_of_the_game_within_its_budget_and_counts_what_it_does_not_know() {
    use fmv_ff7::script::scene::{Limits, SceneVars, Vm};
    use std::collections::BTreeMap;
    let Some(dir) = data_dir() else {
        eprintln!("FMV_FF7_DATA is not set; skipping");
        return;
    };
    let mut lgp = LgpArchive::open_path(flevel_path(&dir)).unwrap();
    let entries: Vec<(usize, String)> =
        lgp.list().iter().enumerate().filter(|(_, e)| script_common::is_field_name(&e.name)).map(|(i, e)| (i, e.name.clone())).collect();
    let limits = Limits { steps: 4_000, windows: 16, threads: 16, frames: 1_800 };
    let (mut fields, mut runs, mut windows) = (0usize, 0usize, 0usize);
    let mut ends: BTreeMap<String, usize> = BTreeMap::new();
    let mut unknown: BTreeMap<&'static str, u32> = BTreeMap::new();
    let mut ignored: BTreeMap<&'static str, u32> = BTreeMap::new();
    for (index, _name) in &entries {
        let Ok(stored) = lgp.read_index(*index) else { continue };
        let Ok(bytes) = fmv_ff7::lzss::decompress(&stored) else { continue };
        let Ok(field) = FieldFile::parse(bytes) else { continue };
        let Ok(script) = FieldScript::from_field(&field) else { continue };
        fields += 1;
        for entity in 0..script.entity_count() {
            for slot in 1..fmv_ff7::script::SCRIPTS_PER_ENTITY {
                if script.span(entity, slot).is_none() {
                    continue;
                }
                let (Ok(e), Ok(s)) = (u8::try_from(entity), u8::try_from(slot)) else { continue };
                let mut vm = Vm::new(SceneVars::new(StoryState::new_game(), &[]), 1, limits);
                vm.begin(vec![Step::Start(ScriptRef::new(e, s))]);
                runs += 1;
                let mut resumes = 0;
                let end = loop {
                    let beat = vm.resume(&script, None);
                    resumes += 1;
                    assert!(resumes <= 200, "a scene that never ends: {_name} entity {entity} slot {slot}");
                    match beat.then {
                        Then::End { how, .. } => break how,
                        Then::Window(_) => windows += 1,
                    }
                };
                let key = match end {
                    End::Finished => "Finished",
                    End::Budget => "Budget",
                    End::Broken => "Broken",
                    End::MapJump { .. } => "MapJump",
                    End::Battle => "Battle",
                    End::MiniGame => "MiniGame",
                    End::GameOver => "GameOver",
                };
                *ends.entry(key.to_string()).or_default() += 1;
                for (op, n) in &vm.stats().unknown {
                    *unknown.entry(op).or_default() += n;
                }
                for (op, n) in &vm.stats().ignored {
                    *ignored.entry(op).or_default() += n;
                }
            }
        }
    }
    eprintln!("{fields} fields, {runs} scripts run as a scene root, {windows} windows handed out; ends: {ends:?}");
    eprintln!("unknown opcodes (count over all runs): {unknown:?}");
    eprintln!("skipped as known to change nothing a scene shows: {} distinct opcodes", ignored.len());
    assert!(fields >= 700 && runs > 20_000, "{fields} fields, {runs} runs");
    assert_eq!(ends.values().sum::<usize>(), runs, "every run ended");
}
