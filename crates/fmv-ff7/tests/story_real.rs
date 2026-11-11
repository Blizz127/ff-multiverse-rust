//! Checks of the FF7 story model (`fmv_ff7::story`) against the owner's REAL FF7 data (FF7 Steam
//! Edition, English).
//!
//! Every test is `#[ignore]` and also returns early when `FMV_FF7_DATA` is unset, so a normal
//! `cargo test` never touches game data. Run them with
//!
//! ```text
//! FMV_FF7_DATA=<.../data> cargo test -p fmv-ff7 --test story_real -- --ignored --nocapture
//! ```
//!
//! The files are only opened read-only; what is printed is counts, field names and numbers.

use fmv_adapter_api::{AreaRuntime, GameAdapter, StoryTrigger};
use fmv_core::{EditionProvenance, GameId, PhaseKey, Platform, Pos};
use fmv_ff7::field::FieldFile;
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::script::FieldScript;
use fmv_ff7::story::{merge_progression, AreaStory, StoryState, NEW_GAME_MOMENT};
use fmv_ff7::{lzss, Ff7Adapter};
use std::collections::BTreeMap;
use std::path::PathBuf;

fn data_dir() -> Option<PathBuf> {
    match std::env::var_os("FMV_FF7_DATA") {
        Some(dir) => Some(PathBuf::from(dir)),
        None => {
            eprintln!("FMV_FF7_DATA is not set; skipping");
            None
        }
    }
}

fn flevel(dir: &std::path::Path) -> PathBuf {
    dir.join("field").join("flevel.lgp")
}

fn adapter(dir: &std::path::Path) -> Ff7Adapter {
    let provenance = EditionProvenance {
        game: GameId::Ff7,
        platform: Platform::Pc,
        edition: "steam".into(),
        region: "any".into(),
        language: "en".into(),
        revision: "test".into(),
        sources: vec![],
        modded: false,
    };
    Ff7Adapter::from_flevel_path(flevel(dir), provenance).unwrap()
}

fn at(moment: u16) -> Vec<u8> {
    StoryState::at(moment).encode()
}

/// Full source evaluation of the ordinary points in the opening/reactor route. The two
/// points ending in MAPJUMP are separate authored-scene continuations; they are checked
/// below for source admission and for refusal by this non-travel evaluator.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn exact_ordinary_route_points_keep_all_writes_and_effective_battle_rules() {
    use fmv_adapter_api::StoryBattlePolicy;
    use fmv_ff7::story::compare_exchange_progression;
    let Some(dir) = data_dir() else { return };
    let adapter = adapter(&dir);
    let cases = [
        ("md1stin", 0, 1, 0, 6, 0, None, Some((300, false))),
        ("md1_2", 0, 7, 0, 8, 0, None, None),
        ("nrthmk", 0, 8, 0, 10, 0, None, None),
        ("nmkin_1", 0, 10, 0, 11, 0, None, None),
        ("elevtr1", 0, 11, 0, 12, 1, None, None),
        ("nmkin_2", 0, 12, 1, 13, 1, None, None),
        ("nmkin_3", 0, 13, 1, 14, 1, None, None),
        ("nmkin_5", 0, 14, 1, 27, 65, Some(600), Some((324, true))),
        ("nmkin_3", 0, 27, 1, 27, 33, None, None),
        ("elevtr1", 1, 27, 33, 27, 32, None, None),
    ];
    for (key, point, moment, flags, next_moment, next_flags, countdown, battle) in cases {
        let area = adapter.load_field(key).unwrap();
        let script = FieldScript::from_field(area.field_file()).unwrap();
        let mut story = AreaStory::scan(&script, area.walkmesh());
        story.add_talk_points(&script, &area.npcs());
        let mut before = StoryState::fresh_first_control();
        before.moment = moment;
        before.write_byte(1, 225, flags).unwrap();
        let expected = before.encode();
        let step = story.advance_exact_with(&script, &expected, point)
            .unwrap_or_else(|error| panic!("{key}#{point}: {error:?}"))
            .unwrap_or_else(|| panic!("{key}#{point} did not apply"));
        let after = StoryState::decode(&step.progress).unwrap();
        assert_eq!((after.moment, after.var(1, 225), step.countdown), (next_moment, next_flags, countdown), "{key}#{point}");
        assert_eq!(compare_exchange_progression(&expected, &expected, &step.progress), Some(step.progress.clone()), "{key}#{point}");
        assert!(compare_exchange_progression(&step.progress, &expected, &step.progress).is_none(), "{key}#{point}: stale predecessor refuses");
        let policy = battle.map(|(formation, victory_fanfare)| StoryBattlePolicy { formation, escape_forbidden: false, victory_fanfare, game_over_on_defeat: true });
        assert_eq!(area.story_battle_policy(&expected, point), policy, "{key}#{point}: only the mode at BATTLE matters");
        if key == "nmkin_5" {
            assert_eq!(after.known_bit(1, 226, 2), Some(true), "the non-curated pre-trigger assignment survives");
        }
    }
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn terminal_points_require_their_authored_scene_and_optional_materia_cannot_disappear() {
    use fmv_ff7::story::StoryPointSource;
    let Some(dir) = data_dir() else { return };
    let adapter = adapter(&dir);
    for (key, point, moment, flags, entity) in [("md1_1", 0, 6, 0, 0), ("nrthmk", 1, 27, 32, 7)] {
        let area = adapter.load_field(key).unwrap();
        let script = FieldScript::from_field(area.field_file()).unwrap();
        let story = AreaStory::scan(&script, area.walkmesh());
        let mut before = StoryState::fresh_first_control();
        before.moment = moment;
        before.write_byte(1, 225, flags).unwrap();
        assert_eq!(area.story_point_source(&before.encode(), point), Some(StoryPointSource { entity, slot: 0, main: true }), "{key}");
        assert!(story.advance_exact_with(&script, &before.encode(), point).is_err(), "{key}: MAPJUMP must be committed through its full scene/landing path");
    }
    let area = adapter.load_field("nmkin_5").unwrap();
    let mut before = StoryState::fresh_first_control();
    before.moment = 14;
    assert!(area.story_advance(&before.encode(), 3).is_none(), "the optional Restore materia cannot become a moment-only save");
}

fn corners(area: &dyn AreaRuntime, id: u32) -> Vec<Pos> {
    let point = area.story_points().into_iter().find(|p| p.id == id).expect("the point exists");
    let StoryTrigger::Region(corners) = point.trigger else { panic!("a region") };
    corners
}

/// The first field of the first mission (`md1stin`, the Sector 1 station): the director's loop sets the
/// counter 1 -> 6 when the player's character steps onto triangle 12.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn md1stin_has_exactly_the_station_trigger_and_new_accounts_can_use_it() {
    let Some(dir) = data_dir() else { return };
    let a = adapter(&dir);
    let area = a.load_area("md1stin").unwrap();
    let points = area.story_points();
    assert_eq!(points.len(), 2, "the station trigger, and the intro's own step: {points:?}");
    assert!(matches!(points[1].trigger, StoryTrigger::Enter) && points[1].label == "entry: moment ==0 -> 1", "{points:?}");
    let point = &points[0];
    println!("md1stin story point: {}", point.label);
    assert!(point.label.contains("triangle 12") && point.label.contains("moment ==1 -> 6"), "{}", point.label);
    assert!(point.label.contains("battle 300"), "the script's battle is named, not run: {}", point.label);

    // The region is the walkmesh's own triangle 12.
    let tris = area.walk_triangles();
    let region = corners(area.as_ref(), point.id);
    assert_eq!(region, tris[12].to_vec(), "the region is walkmesh triangle 12");

    // A new account (the empty blob) is at the moment the point needs; the step is 1 -> 6, once.
    assert_eq!(NEW_GAME_MOMENT, 1);
    let step = area.story_advance(&[], point.id).expect("a new account advances here");
    assert_eq!(step.progress, at(6));
    assert_eq!(step.summary, "moment=1->6");
    assert_eq!(area.story_advance(&step.progress, point.id), None, "only once");
    for moment in [0u16, 2, 7, 8, 12, 1600] {
        assert_eq!(area.story_advance(&at(moment), point.id), None, "moment {moment} cannot");
    }

    // Nobody spawns on it (a spawn inside would advance a player on its first step).
    for spawn in area.spawn_points() {
        assert!(!point.is_hit_by(spawn, spawn), "spawn {spawn:?} is inside the region");
    }

    // The default phase is the empty key; after the step the player is in another phase of md1stin.
    assert_eq!(area.phase_key(&[]), Some(PhaseKey::default()));
    assert_eq!(area.phase_key(&at(1)), Some(PhaseKey::default()));
    let advanced = area.phase_key(&at(6)).unwrap();
    assert_ne!(advanced, PhaseKey::default(), "the story moved on for this field");
    assert_eq!(area.phase_key(&at(7)), Some(advanced.clone()), "6 and 7 look the same to this field");
    assert_ne!(area.phase_key(&at(0)), area.phase_key(&at(1)), "the intro and the first moment differ");
    assert_eq!(area.phase_key(&[7, 7]), None, "a blob that is not ours");
}

/// `md1_1`: the same shape with `<`: stepping onto triangle 62 while the counter is below 7 sets it to 7.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn md1_1_has_the_second_trigger_of_the_first_mission() {
    let Some(dir) = data_dir() else { return };
    let a = adapter(&dir);
    let area = a.load_area("md1_1").unwrap();
    let points = area.story_points();
    assert_eq!(points.len(), 1, "{points:?}");
    println!("md1_1 story point: {}", points[0].label);
    assert!(points[0].label.contains("triangle 62") && points[0].label.contains("-> 7"), "{}", points[0].label);
    assert_eq!(area.story_advance(&[], 0).unwrap().progress, at(7), "a new account steps 1 -> 7 here");
    assert_eq!(area.story_advance(&at(6), 0).unwrap().progress, at(7));
    assert_eq!(area.story_advance(&at(7), 0), None);
    // md1_1 compares the counter with 7 only: the moments below it are one phase (the default's), 7 and up another.
    assert_eq!(area.phase_key(&at(6)), Some(PhaseKey::default()));
    assert_ne!(area.phase_key(&at(7)), Some(PhaseKey::default()));
}

/// Over every field of the archive: the scan never fails, every field's key for a new account is
/// the empty key, and the story points are the few the pattern allows.
#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn every_field_scans_and_a_new_account_is_in_the_empty_phase_of_all_of_them() {
    let Some(dir) = data_dir() else { return };
    let mut lgp = LgpArchive::open_path(flevel(&dir)).unwrap();
    let names: Vec<String> = lgp.list().iter().map(|e| e.name.clone()).filter(|n| !n.contains('.') && !n.eq_ignore_ascii_case("maplist")).collect();
    assert!(names.len() >= 700, "{} fields", names.len());
    let mut with_points: BTreeMap<String, usize> = BTreeMap::new();
    let (mut with_edges, mut total_edges) = (0usize, 0usize);
    for name in &names {
        let stored = lgp.read(name).unwrap();
        let field = FieldFile::parse(lzss::decompress(&stored).unwrap()).unwrap();
        let script = FieldScript::from_field(&field).unwrap();
        let mesh = field.walkmesh().unwrap();
        let story = AreaStory::scan(&script, &mesh);
        assert_eq!(story.phase_key(&[]), Some(PhaseKey::default()), "{name}: a new account is the empty phase");
        assert_eq!(story.phase_key(&at(NEW_GAME_MOMENT)), Some(PhaseKey::default()), "{name}");
        if story.point_count() > 0 {
            with_points.insert(name.clone(), story.point_count());
            for p in story.points() {
                println!("  story point {name}#{}: {}", p.id, p.label);
            }
        }
        if !story.edges().is_empty() {
            with_edges += 1;
            total_edges += story.edges().len();
        }
        // Every point is one that a step can actually reach: its triangle exists and its corners are finite.
        for p in story.points() {
            match &p.trigger {
                // (a triangle that also waits for OK, the lift's panel, is a press)
                StoryTrigger::Region(c) | StoryTrigger::Press(c) => {
                    assert_eq!(c.len(), 3);
                    assert!(c.iter().all(|p| p.is_finite()));
                }
                StoryTrigger::Line(a, b) => assert!(a.is_finite() && b.is_finite() && (a.x, a.y) != (b.x, b.y), "{name}#{}", p.id),
                StoryTrigger::Enter => {}
                other => panic!("{name}#{}: {other:?}", p.id),
            }
        }
    }
    println!("{} fields scanned; {with_edges} cut the counter into phases ({total_edges} edges in all)", names.len());
    println!("fields with story points ({}): {with_points:?}", with_points.len());
    assert!(with_points.contains_key("md1stin") && with_points.contains_key("md1_1"), "{with_points:?}");
}

#[test]
#[ignore = "needs the owner's FF7 data (FMV_FF7_DATA)"]
fn the_adapter_merges_progressions_like_the_story_module() {
    let Some(dir) = data_dir() else { return };
    let a = adapter(&dir);
    assert_eq!(a.merge_progression(&[], &at(6)), at(6));
    assert_eq!(a.merge_progression(&at(7), &at(6)), at(7));
    assert_eq!(merge_progression(&[7, 7], &at(9)), vec![7, 7]);
    assert_eq!(a.merge_progression(&[7, 7], &at(9)), vec![7, 7]);
}
