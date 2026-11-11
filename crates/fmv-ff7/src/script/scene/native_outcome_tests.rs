//! Native-party scene effects, publication, and replay over invented scripts only.

use super::*;
use crate::field::FieldFile;
use crate::script::dialogue::End;
use crate::script::synth::{dialog, SynthScript};
use crate::story::{NativePartyState, StoryState};
use crate::testutil::FieldSpec;

fn set_byte(bank: u8, address: u8, value: u8) -> Vec<u8> {
    vec![0x80, bank << 4, address, value]
}

fn message() -> Vec<u8> { vec![0x40, 0, 0] }

fn battle() -> Vec<u8> { vec![0x70, 0, 72, 1] }

fn if_byte(bank: u8, address: u8, value: u8, body: Vec<u8>) -> Vec<u8> {
    let mut code = vec![0x14, bank << 4, address, value, 0, u8::try_from(body.len() + 1).unwrap()];
    code.extend(body);
    code
}

fn field(body: Vec<Vec<u8>>) -> FieldFile {
    field_with_setup(vec![], body)
}

fn field_with_setup(setup: Vec<Vec<u8>>, body: Vec<Vec<u8>>) -> FieldFile {
    let code = [body.concat(), vec![0]].concat();
    let setup = [setup.concat(), vec![0]].concat();
    let section = SynthScript { dialogs: vec![dialog("A native scene.")], ..Default::default() }
        .entity("actor", vec![(0, setup), (1, code)]).build();
    FieldFile::parse(FieldSpec { overrides: vec![(0, section)], ..Default::default() }.build()).unwrap()
}

fn definition() -> SceneDef {
    SceneDef {
        id: "test/native-outcome".into(), game: "ff7".into(), field: "fixture".into(),
        trigger: Trigger { entity: 0, name: "actor".into(), slot: 1, talk_radius: 80, moments: 0..=u16::MAX },
        setup: vec![], given: vec![], root: vec![Step::Await(ScriptRef::new(0, 1))], cast: vec![],
        speakers: vec![], witness: vec![], arrival: Spot { x: 0, y: 0, triangle: 0, direction: 0 },
        presents: None, presents_bit: None, resume_after_battle: None,
    }
}

fn story() -> StoryState {
    let mut state = StoryState::at(69).into_native_party().unwrap();
    *state.native_party_mut().unwrap() = NativePartyState::from_parts(
        [0, 1, 255], 7, (0x8003, u16::MAX), (0x8000, u16::MAX),
    ).unwrap();
    for (address, member) in [9, 10, 11].into_iter().zip([0, 1, 255]) {
        state.write_byte(3, address, member).unwrap();
    }
    state.write_masked(3, 120, 0x80, 0x81).unwrap();
    state
}

fn run(state: &StoryState, body: Vec<Vec<u8>>) -> Outcome {
    run_to_end_with_resources_strict(&field(body), &definition(), state, 17,
        Resources { gil: Some(100) }, &[]).unwrap()
}

fn apply(outcome: &Outcome, before: &StoryState) -> StoryState {
    assert!(outcome.is_complete(), "{outcome:?}");
    assert!(outcome.unknown_persistent_reads.is_empty(), "{outcome:?}");
    let mut after = before.clone();
    outcome.writes.apply_extended_to(&mut after, before).expect("a sealed native outcome applies");
    assert!(outcome.party_is_the_storys(before, &after));
    after
}

#[test]
fn native_outcome_applies_interleaved_raw_and_party_writes_atomically() {
    let before = story();
    let result = run(&before, vec![
        set_byte(4, 10, 2), // Raw [Cloud, Tifa, empty], main still [Cloud, Barret, empty].
        vec![0xC9, 1], // An absent removal still synchronizes main from raw.
        vec![0xC8, 1], // The now-missing Barret takes the last raw/main slot.
        vec![0xCD, 0, 2], // Availability removal clears raw Tifa only; main keeps her.
        vec![0xCE, 2],
        set_byte(3, 10, 3), // An ordinary assignment changes raw, never main.
        set_byte(1, 40, 27),
    ]);
    let after = apply(&result, &before);
    assert_eq!(after.native_party().unwrap().known_main(), Some([0, 2, 1]));
    assert_eq!([9, 10, 11].map(|address| after.var(3, address)), [0, 3, 1]);
    assert_eq!(after.native_party().unwrap().availability_bits(), (0x8003, u16::MAX));
    assert_eq!(after.native_party().unwrap().locking_bits(), (0x8004, u16::MAX));
    assert_eq!((after.var(3, 120), after.known_mask(3, 120)), (0x80, 0x81));
    assert_eq!(after.var(1, 40), 27);
    assert_eq!(after.moment, before.moment);
    assert!(result.party.is_empty(), "native operations never manufacture legacy table journals");
    let journal = result.writes.native_party.as_ref().unwrap();
    assert_eq!(journal.before(), &before);
    assert_eq!(journal.after(), &after);
    assert!(!journal.events().is_empty());

    let mut stale = before.clone();
    stale.write_byte(1, 80, 9).unwrap();
    let held = stale.clone();
    assert_eq!(result.writes.apply_extended_to(&mut stale, &before), None);
    assert_eq!(stale, held, "a stale unrelated byte cannot receive a partial native effect");
    assert_eq!(result.writes.apply_extended_to(&mut stale, &held), None,
        "changing the caller's expected image cannot replace the journal's sealed predecessor");
    assert_eq!(stale, held);

    let mut tampered = result.writes.clone();
    tampered.persistent.insert((3, 10), (7, 255));
    tampered.moment = Some(999);
    let mut untouched = before.clone();
    assert_eq!(tampered.apply_extended_to(&mut untouched, &before), Some(true));
    assert_eq!(untouched, after, "inspection summaries cannot override the sealed ordered journal");
}

#[test]
fn native_party_only_and_knowledge_only_effects_are_changes_without_a_moment_write() {
    let before = story();
    let locked = run(&before, vec![vec![0xCE, 2]]);
    assert!(!locked.writes.is_story_neutral());
    assert_eq!(locked.writes.moment, None);
    assert!(locked.writes.persistent.is_empty());
    let after = apply(&locked, &before);
    assert_ne!(after, before);
    assert_eq!(after.moment, before.moment);
    assert_eq!(after.native_party().unwrap().known_locked(2), Ok(Some(true)));

    let unknown = StoryState::at(69).into_native_party().unwrap();
    let unlock = run(&unknown, vec![vec![0xCF, 2]]);
    assert!(!unlock.writes.is_story_neutral(), "known false differs from unknown");
    let after = apply(&unlock, &unknown);
    assert_eq!(after.native_party().unwrap().known_locked(2), Ok(Some(false)));
    assert_eq!(after.native_party().unwrap().known_main(), None, "an independent bit write guesses no party");
    assert_eq!(after.known_mask(3, 9), 0);
}

#[test]
fn native_setup_effects_precede_scene_effects_and_unknown_setup_reads_remain_unsafe() {
    let before = story();
    let source = field_with_setup(vec![set_byte(4, 10, 2), vec![0xC9, 1]], vec![vec![0xC8, 1]]);
    let mut def = definition();
    def.setup = vec![ScriptRef::new(0, 0)];
    let outcome = run_to_end_with_resources_strict(&source, &def, &before, 17,
        Resources { gil: Some(100) }, &[]).unwrap();
    let after = apply(&outcome, &before);
    assert_eq!(after.native_party().unwrap().known_main(), Some([0, 2, 1]));
    assert_eq!([9, 10, 11].map(|address| after.var(3, address)), [0, 2, 1]);
    assert_eq!(outcome.setup_stats.handled.get("PRTYM"), Some(&1));
    assert_eq!(outcome.stats.handled.get("PRTYP"), Some(&1));
    let journal = outcome.writes.native_party.as_ref().unwrap();
    assert_eq!(journal.before(), &before);
    assert_eq!(journal.after(), &after);
    assert!(journal.any_change());

    let source = field_with_setup(vec![if_byte(1, 100, 0, set_byte(5, 3, 1))], vec![vec![0xCE, 2]]);
    let mut play = ScenePlay::with_resources(&source, &def, &before, 17, Resources { gil: Some(100) }).unwrap();
    play.next_strict(None).unwrap();
    assert!(play.snapshot().unwrap().unknown_persistent_reads.contains(&(1, 100)));
    assert_eq!(play.current_story(), None, "taking setup statistics must not erase an unknown setup read");
    let uncertain = run_to_end_with_resources_strict(&source, &def, &before, 17,
        Resources { gil: Some(100) }, &[]).unwrap();
    assert!(!uncertain.same_story_as(&uncertain), "a guessed native branch is not a complete replacement proof");
}

#[test]
fn replacement_equivalence_preserves_ordered_native_effects_even_when_the_final_image_matches() {
    let mut before = story();
    before.native_party_mut().unwrap().write_availability_masked(4, 4);
    let original = run(&before, vec![vec![0xC8, 2], vec![0xC9, 2]]);
    let same = run(&before, vec![message(), vec![0xC8, 2], vec![0xC9, 2]]);
    let omitted = run(&before, vec![]);
    assert_eq!(apply(&original, &before), before);
    assert_eq!(apply(&same, &before), before);
    assert_eq!(apply(&omitted, &before), before);
    assert!(original.same_story_as(&same) && same.same_story_as(&original));
    assert!(!original.same_story_as(&omitted) && !omitted.same_story_as(&original),
        "temporary native assignments are part of the story contract even when the final tuple matches");
}

#[test]
fn native_battle_boundary_captures_prefix_and_rejects_every_tampered_afterimage_without_advancing() {
    let before = story();
    let field = field(vec![vec![0xC8, 2], set_byte(1, 40, 12), vec![0x72, 8, 0], battle()]);
    let mut play = ScenePlay::with_resources(&field, &definition(), &before, 17, Resources { gil: Some(100) }).unwrap();
    play.next_strict(None).unwrap();
    let Some(SceneBoundary::Battle(checkpoint)) = play.boundary() else { panic!("expected battle") };
    let at_battle = play.current_story().expect("the exact prefix has a complete story image");
    assert_eq!(at_battle.native_party().unwrap().known_main(), Some([0, 1, 2]));
    assert_eq!(at_battle.native_party().unwrap().availability_bits(), (0x8007, u16::MAX));
    assert_eq!(at_battle.var(1, 40), 12);
    assert_eq!(before.native_party().unwrap().known_main(), Some([0, 1, 255]));
    let snapshot = play.snapshot();
    let transcript = play.transcript().to_vec();
    let mutations: &[fn(&mut StoryState)] = &[
        |s| { s.native_party_mut().unwrap().write_main_slots([0, 2, 1], 7).unwrap(); },
        |s| { s.native_party_mut().unwrap().write_availability_masked(0, 4); },
        |s| { s.native_party_mut().unwrap().write_locks_masked(4, 4); },
        |s| { s.write_byte(4, 10, 7).unwrap(); },
        |s| { s.write_byte(1, 40, 13).unwrap(); },
        |s| { s.write_byte(1, 41, 0).unwrap(); },
        |s| { s.moment += 1; },
    ];
    for (index, mutate) in mutations.iter().enumerate() {
        let mut tampered = at_battle.clone();
        mutate(&mut tampered);
        assert_ne!(tampered, at_battle, "mutation {index} must change the image");
        assert_eq!(play.resume_battle_checked(BattleResume {
            checkpoint, resolution: BattleResolution::Victory, story: tampered, resources: Resources { gil: Some(125) },
        }), Err(SceneError::BattleStory), "mutation {index}");
        assert_eq!(play.snapshot(), snapshot);
        assert_eq!(play.current_story(), Some(at_battle.clone()));
        assert_eq!(play.transcript(), transcript);
    }
    assert_eq!(play.resume_battle_checked(BattleResume {
        checkpoint, resolution: BattleResolution::Victory, story: StoryState::at(69).into_extended().unwrap(),
        resources: Resources { gil: Some(125) },
    }), Err(SceneError::NativePartyUnsupported));
    assert_eq!(play.snapshot(), snapshot);
    play.resume_battle_checked(BattleResume {
        checkpoint, resolution: BattleResolution::Victory, story: at_battle.clone(), resources: Resources { gil: Some(125) },
    }).unwrap();
    assert_eq!(play.current_story(), Some(at_battle.clone()));
    assert!(play.writes().is_story_neutral());
    let rebased = play.writes();
    let journal = rebased.native_party.as_ref().unwrap();
    assert_eq!(journal.before(), &at_battle);
    assert_eq!(journal.after(), &at_battle);
    assert!(journal.events().is_empty(), "the committed prefix is not emitted again");
}

#[test]
fn native_battle_resume_cannot_clear_an_uninterpreted_prefix() {
    let before = story();
    let field = field(vec![vec![0x7F, 0, 5], vec![0x72, 8, 0], battle()]);
    let mut play = ScenePlay::with_resources(&field, &definition(), &before, 17, Resources { gil: Some(100) }).unwrap();
    play.next_strict(None).unwrap();
    let Some(SceneBoundary::Battle(checkpoint)) = play.boundary() else { panic!("expected battle") };
    assert!(!play.stats().unknown.is_empty());
    assert_eq!(play.current_story(), None, "uninterpreted instructions cannot certify an exact boundary image");
    let snapshot = play.snapshot();
    assert_eq!(play.resume_battle_checked(BattleResume {
        checkpoint, resolution: BattleResolution::Victory, story: before, resources: Resources { gil: Some(125) },
    }), Err(SceneError::BattleStory));
    assert_eq!(play.snapshot(), snapshot);
    assert!(play.transcript().is_empty());
}

#[test]
fn native_battle_rebase_retains_scratch_rng_and_replays_only_new_ordered_effects() {
    let before = story();
    let prefix = vec![vec![0x72, 8, 0], set_byte(5, 9, 77), vec![0x99, 5, 2], vec![0xC8, 2]];
    let suffix = vec![vec![0xC9, 1], set_byte(4, 10, 3), vec![0x99, 5, 3], if_byte(5, 9, 77, message())];
    let battle_field = field([prefix.clone(), vec![battle()], suffix.clone()].concat());
    let control = field([prefix, suffix].concat());
    let def = definition();
    let resources = Resources { gil: Some(100) };
    let mut play = ScenePlay::with_resources(&battle_field, &def, &before, 123, resources).unwrap();
    let identity = play.replay_identity();
    play.next_strict(None).unwrap();
    let Some(SceneBoundary::Battle(checkpoint)) = play.boundary() else { panic!("expected battle") };
    let committed = play.current_story().unwrap();
    play.resume_battle_checked(BattleResume {
        checkpoint, resolution: BattleResolution::Victory, story: committed.clone(), resources: Resources { gil: Some(125) },
    }).unwrap();
    play.next_strict(None).unwrap();
    let Some(SceneBoundary::Window(window)) = play.boundary() else { panic!("scratch must survive the battle") };
    play.next_strict(Some(WindowAck { checkpoint: window, answer: None })).unwrap();
    assert_eq!(play.boundary(), Some(SceneBoundary::End(End::Finished)));
    let final_story = play.current_story().unwrap();
    assert_eq!(final_story.native_party().unwrap().known_main(), Some([0, 255, 2]));
    assert_eq!([9, 10, 11].map(|address| final_story.var(3, address)), [0, 3, 2]);
    let writes = play.writes();
    let journal = writes.native_party.as_ref().unwrap();
    assert_eq!(journal.before(), &committed);
    assert_eq!(journal.after(), &final_story);
    assert!(!journal.events().is_empty());
    let mut published = committed.clone();
    assert_eq!(writes.apply_extended_to(&mut published, &committed), Some(true));
    assert_eq!(published, final_story);

    let uninterrupted = run_to_end_with_resources_strict(&control, &def, &before, 123, resources, &[]).unwrap();
    assert_eq!(apply(&uninterrupted, &before), final_story);
    for key in [(5, 2), (5, 3), (5, 9)] {
        assert!(writes.untracked.contains_key(&key), "the resumed run must actually produce scratch/RNG {key:?}");
        assert!(uninterrupted.writes.untracked.contains_key(&key), "the control run must actually produce scratch/RNG {key:?}");
        assert_eq!(writes.untracked.get(&key), uninterrupted.writes.untracked.get(&key), "scratch/RNG {key:?}");
    }
    assert_eq!((play.resources().gil_before, play.resources().gil_after), (Some(125), Some(125)));
    let replayed = replay_scene_to(&battle_field, &def, &before, resources, identity, play.transcript(),
        SceneBoundary::End(End::Finished)).unwrap();
    assert_eq!(replayed.snapshot(), play.snapshot());
    assert_eq!(replayed.current_story(), Some(final_story));
}

/// Reads the original field archive only. The explicit native test profile establishes its
/// own inputs; this does not infer party resources from an imported save or enable admission.
#[test]
#[ignore = "needs the owner's original FF7 field archive (FMV_FF7_DATA)"]
fn owned_reactor_door_establishes_native_barret_join_with_an_exact_replay() {
    use crate::lgp::LgpArchive;
    use crate::story::{NativePartyOp, NativeStoryEvent};

    let data = std::path::PathBuf::from(std::env::var_os("FMV_FF7_DATA").expect("owned FF7 data path"));
    let mut archive = LgpArchive::open_path(data.join("field/flevel.lgp")).expect("read-only original archive");
    let bytes = archive.read("nmkin_1").expect("original reactor-door field");
    let source = FieldFile::parse(crate::lzss::decompress(&bytes).unwrap()).unwrap();
    let def = reactor_door_scene();
    let mut before = StoryState::fresh_native_first_control();
    before.moment = 10;
    assert_eq!(before.native_party().unwrap().known_main(), Some([0, 255, 255]));
    assert_eq!([9, 10, 11].map(|address| (before.var(3, address), before.known_mask(3, address))),
        [(0, 255), (255, 255), (255, 255)]);
    assert_eq!(before.native_party().unwrap().availability_bits(), (1, u16::MAX));
    let locks = before.native_party().unwrap().locking_bits();
    let resources = Resources { gil: Some(600) };
    let mut play = ScenePlay::with_resources(&source, &def, &before, 1, resources).unwrap();
    let identity = play.replay_identity();
    play.next_strict(None).unwrap();
    let mut windows = 0;
    loop {
        match play.boundary().expect("the original script reaches a boundary") {
            SceneBoundary::Window(checkpoint) => {
                windows += 1;
                assert!(windows <= 16, "the bounded door scene must terminate");
                assert_eq!(checkpoint.choices, None);
                play.next_strict(Some(WindowAck { checkpoint, answer: None })).unwrap();
            }
            SceneBoundary::End(end) => {
                assert_eq!(end, End::Finished);
                break;
            }
            SceneBoundary::Battle(_) => panic!("the door talk contains no battle"),
        }
    }
    assert_eq!(windows, 6);
    let after = play.current_story().expect("source-backed native state is fully evaluated");
    assert_eq!(after.moment, 11);
    assert_eq!(after.native_party().unwrap().known_main(), Some([0, 1, 255]));
    assert_eq!([9, 10, 11].map(|address| (after.var(3, address), after.known_mask(3, address))),
        [(0, 255), (1, 255), (255, 255)]);
    assert_eq!(after.native_party().unwrap().availability_bits(), (3, u16::MAX));
    assert_eq!(after.native_party().unwrap().locking_bits(), locks, "the party assignment changes no lock knowledge");
    assert_eq!((play.resources().gil_before, play.resources().gil_after), (Some(600), Some(600)));
    let writes = play.writes();
    let journal = writes.native_party.as_ref().unwrap();
    assert_eq!(journal.before(), &before);
    assert_eq!(journal.after(), &after);
    assert!(journal.events().iter().any(|event| matches!(event,
        NativeStoryEvent::Party(effect) if matches!(effect.operation(), NativePartyOp::Set([0, 1, 254 | 255])))),
        "the real PRTYE, not a moment-table projection, establishes Barret");
    let mut published = before.clone();
    assert_eq!(writes.apply_extended_to(&mut published, &before), Some(true));
    assert_eq!(published, after);

    let outcome = run_to_end_with_resources_strict(&source, &def, &before, 1, resources, &[]).unwrap();
    assert!(outcome.is_complete() && outcome.unknown_persistent_reads.is_empty());
    assert!(outcome.party.is_empty());
    assert_eq!(apply(&outcome, &before), after);
    assert_eq!(outcome.writes.native_party.as_ref(), Some(journal));
    let again = run_to_end_with_resources_strict(&source, &def, &before, 1, resources, &[]).unwrap();
    assert!(outcome.same_story_as(&again) && again.same_story_as(&outcome));
    let replayed = replay_scene_to(&source, &def, &before, resources, identity, play.transcript(),
        SceneBoundary::End(End::Finished)).unwrap();
    assert_eq!(replayed.snapshot(), play.snapshot());
    assert_eq!(replayed.current_story(), Some(after));
}
