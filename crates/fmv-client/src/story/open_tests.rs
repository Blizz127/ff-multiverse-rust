//! The story bot of a bot that also travels (`StoryBot::set_wanted_exit`): a step that OPENS the exit its route goes to is
//! pursued even when it is not forward, and one that would CLOSE it is not even when it is. The case is the lift of the
//! first reactor (a stub of it below): a panel whose press rides down (the lift bit on: forward) when the lift is up and
//! rides back up (the bit off: not forward) when it is down, and two exits, up while the lift is up and down while it is down.

use super::*;
use fmv_adapter_api::{AdapterError, AreaInfo, AreaRuntime, Bounds, ExitKind, GameAdapter, StoryStep};
use fmv_core::{AreaId, EditionProvenance, Fingerprint, GameId, Platform};
use fmv_ff7::story::StoryState;

const UP: &str = "above";
const DOWN: &str = "below";

/// The story with the lift up (moment 27) and down (the bit on).
fn lift(down: bool) -> Vec<u8> {
    let mut s = StoryState::at(27);
    if down {
        assert!(s.set_bit(1, 225, 0, true));
    }
    s.encode()
}

fn is_down(progress: &[u8]) -> bool {
    StoryState::decode(progress).is_some_and(|s| s.known_bit(1, 225, 0) == Some(true))
}

/// A walkable square with a panel in the middle (a press region) and the two lift exits on the west and east walls.
struct Shaft {
    info: AreaInfo,
}

impl Shaft {
    fn new() -> Self {
        Self {
            info: AreaInfo {
                id: AreaId { game: GameId::Ff7, key: "shaft".into() },
                display_name: "shaft".into(),
                fingerprint: Fingerprint([1; 32]),
                provenance: EditionProvenance {
                    game: GameId::Ff7,
                    platform: Platform::Synthetic,
                    edition: "test".into(),
                    region: "none".into(),
                    language: "none".into(),
                    revision: "test".into(),
                    sources: vec![],
                    modded: false,
                },
                fixture: true,
            },
        }
    }

    fn exit(target: &str, x: f32) -> Exit {
        Exit {
            line: (Pos::new(x, -100.0, 0.0), Pos::new(x, 100.0, 0.0)),
            target: target.into(),
            arrival: Pos::default(),
            arrival_facing: None,
            arrival_hint: None,
            kind: ExitKind::Script { id: u32::from(target == DOWN) },
        }
    }
}

impl AreaRuntime for Shaft {
    fn info(&self) -> &AreaInfo {
        &self.info
    }
    fn spawn_points(&self) -> Vec<Pos> {
        vec![Pos::new(0.0, -60.0, 0.0)]
    }
    fn validate_move(&self, _from: Pos, to: Pos) -> MoveResult {
        if to.x.abs() <= 300.0 && to.y.abs() <= 200.0 {
            MoveResult::Accepted(Pos::new(to.x, to.y, 0.0))
        } else {
            MoveResult::Corrected(Pos::new(0.0, -60.0, 0.0))
        }
    }
    fn bounds(&self) -> Bounds {
        Bounds { min: Pos::new(-300.0, -200.0, 0.0), max: Pos::new(300.0, 200.0, 0.0) }
    }
    fn walk_triangles(&self) -> Vec<[Pos; 3]> {
        vec![[Pos::new(-300.0, -200.0, 0.0), Pos::new(300.0, -200.0, 0.0), Pos::new(300.0, 200.0, 0.0)], [Pos::new(-300.0, -200.0, 0.0), Pos::new(300.0, 200.0, 0.0), Pos::new(-300.0, 200.0, 0.0)]]
    }
    fn max_speed(&self) -> f32 {
        600.0
    }
    fn exits(&self) -> Vec<Exit> {
        vec![Self::exit(UP, -250.0), Self::exit(DOWN, 250.0)]
    }
    /// Above is open while the lift is up, below while it is down.
    fn exit_state(&self, exit: &Exit, progress: &[u8]) -> ExitState {
        match (exit.target.as_str() == UP, is_down(progress)) {
            (true, false) | (false, true) => ExitState::Open,
            _ => ExitState::Closed,
        }
    }
    /// The panel: two presses of one region (the ride down, the ride back), like the first reactor's lift.
    fn story_points(&self) -> Vec<StoryPoint> {
        let region = vec![Pos::new(-20.0, 50.0, 0.0), Pos::new(20.0, 50.0, 0.0), Pos::new(20.0, 90.0, 0.0), Pos::new(-20.0, 90.0, 0.0)];
        vec![
            StoryPoint { id: 0, trigger: StoryTrigger::Press(region.clone()), label: "panel: ride down".into() },
            StoryPoint { id: 1, trigger: StoryTrigger::Press(region), label: "panel: ride back".into() },
        ]
    }
    fn story_advance(&self, progress: &[u8], point: u32) -> Option<StoryStep> {
        let down = is_down(progress);
        let mut next = StoryState::decode(progress)?;
        match (point, down) {
            (0, false) => assert!(next.set_bit(1, 225, 0, true)),
            (1, true) => assert!(next.set_bit(1, 225, 0, false)),
            _ => return None,
        }
        Some(StoryStep { summary: "lift".into(), progress: next.encode(), countdown: None })
    }
}

/// A game whose progressions join like FF7's (the further moment, the union of bits): what tells a forward step.
struct Ff7Join;

impl GameAdapter for Ff7Join {
    fn game(&self) -> GameId {
        GameId::Ff7
    }
    fn list_areas(&self) -> Vec<AreaInfo> {
        Vec::new()
    }
    fn load_area(&self, key: &str) -> Result<Box<dyn AreaRuntime>, AdapterError> {
        Err(AdapterError::AreaNotFound(key.to_string()))
    }
    fn merge_progression(&self, stored: &[u8], advanced: &[u8]) -> Vec<u8> {
        fmv_ff7::story::merge_progression(stored, advanced)
    }
}

fn bot(wanted: Option<&str>) -> StoryBot {
    let mut bot = StoryBot::new(true, false).with_adapter(Some(Arc::new(Ff7Join)));
    bot.set_wanted_exit(wanted);
    bot
}

/// The point the bot would go for with the lift in `down`, if any.
fn goes_for(bot: &StoryBot, down: bool) -> Option<u32> {
    bot.target(&Shaft::new(), &lift(down), Pos::new(0.0, -60.0, 0.0)).map(|t| t.point.id)
}

#[test]
fn without_a_route_only_forward_steps_are_pursued_as_ever() {
    let b = bot(None);
    assert_eq!(goes_for(&b, false), Some(0), "the ride down is forward");
    assert_eq!(goes_for(&b, true), None, "the ride back is not");
    // a route to an area the shaft has no exit to changes nothing
    let b = bot(Some("elsewhere"));
    assert_eq!((goes_for(&b, false), goes_for(&b, true)), (Some(0), None));
}

#[test]
fn going_up_the_ride_back_is_pursued_while_the_lift_is_down_and_the_ride_down_never_is_after_it() {
    let b = bot(Some(UP));
    // the lift is down: the way up is closed, and the ride back (not forward) opens it
    assert_eq!(goes_for(&b, true), Some(1));
    // the lift is up (after the ride): the way up is open, and the ride down (forward again) would close it
    assert_eq!(goes_for(&b, false), None, "the bot does not ride up and down for ever");
    // (a bot that already stands in the panel's region at the moment the story changes has nothing to press either)
    let t = b.target(&Shaft::new(), &lift(false), Pos::new(0.0, 70.0, 0.0));
    assert!(t.is_none());
}

#[test]
fn going_down_the_ride_down_is_pursued_and_the_ride_back_is_not() {
    let b = bot(Some(DOWN));
    assert_eq!(goes_for(&b, false), Some(0), "forward, and it opens the way down");
    assert_eq!(goes_for(&b, true), None, "the way down is open, and the ride back would close it");
}

#[test]
fn a_press_the_bot_makes_for_the_exit_is_made_once_and_the_bot_then_walks_on() {
    // the headless loop: the story bot first while it has a point, then the travel bot
    let shaft = Shaft::new();
    let mut b = bot(Some(UP));
    let mut pos = Pos::new(0.0, -60.0, 0.0);
    let mut progress = lift(true);
    let mut presses = 0;
    for _ in 0..4000 {
        if !b.wants_to_walk(&shaft, &progress, pos) {
            break;
        }
        pos = b.step(&shaft, pos, 0.01, 360.0, &progress).pos;
        if b.wants_press() {
            assert!(b.take_press().is_some());
            presses += 1;
            // the server's answer: the panel's second point, the ride back
            let step = shaft.story_advance(&progress, 1).expect("the ride back applies to a lift that is down");
            progress = step.progress;
        }
    }
    assert_eq!(presses, 1, "one press");
    assert!(!is_down(&progress), "the lift is up");
    assert!(!b.wants_to_walk(&shaft, &progress, pos), "and the story bot has nothing left to do: the travel bot walks to the way up");
    assert!(b.gave_up().is_none());
}
