//! Accepted ordinary presentation origins reconcile prediction without moving spectators.

use super::*;
use crate::scene::{Built, SceneBuilder, SceneCmd};
use crate::scene::tests::{def_in, Asked, Scripted};
use fmv_core::GameId;
use fmv_ff7::script::dialogue::End;
use fmv_ff7::script::scene::{Beat, SceneResources, Then};
use fmv_ff7::story::StoryState;
use fmv_protocol::{SceneOrigin, SceneParticipant, SceneReplay, SceneRole, SceneRunId};
use std::cell::RefCell;
use std::rc::Rc;

const FIELD: &str = "md1stin";
const SCENE: &str = "ff7/md1stin/station-guards";
const RUN: SceneRunId = SceneRunId(7);
type Calls = Rc<RefCell<Vec<Option<SceneOrigin>>>>;

/// Deliberately accepts any supplied metadata: central scene validation must reject invalid
/// origins before even this permissive source can be built or change the own player's pose.
struct Builder { calls: Calls, refuse: bool, field: &'static str }

impl SceneBuilder for Builder {
    fn build(&mut self, scene: &str, story: &StoryState, seed: u32, resources: SceneResources) -> Result<Built, String> {
        self.build_with_origin(scene, story, seed, resources, None)
    }

    fn build_with_origin(&mut self, scene: &str, _: &StoryState, _: u32, _: SceneResources,
        origin: Option<&SceneOrigin>) -> Result<Built, String> {
        self.calls.borrow_mut().push(origin.copied());
        if self.refuse { return Err("the local source cannot bind the accepted pose".into()); }
        let mut def = def_in(self.field);
        def.id = scene.into();
        Ok(Built { def, source: Box::new(Scripted {
            initial: vec![],
            beats: vec![Beat { stage: vec![], then: Then::End { at: 30, how: End::Finished } }].into(),
            asked: Asked::default(),
        }) })
    }

    fn reach(&mut self, _: usize, _: &str) -> Option<f32> { None }
}

fn origin() -> SceneOrigin {
    SceneOrigin { pos: Pos::new(3783.25, 28446.5, 310.25), facing: -0.75, triangle: Some(12) }
}

fn start(role: SceneRole, presentation_origin: Option<SceneOrigin>) -> ServerMsg {
    ServerMsg::SceneStart {
        run: RUN, scene: SCENE.into(), seed: 5, progress: StoryState::fresh_first_control().encode(), gil: 7,
        replay: None, presentation_origin, owner: EntityId(1),
        participants: vec![SceneParticipant { entity: EntityId(1), spot: 0 }, SceneParticipant { entity: EntityId(2), spot: 1 }],
        you: role,
    }
}

fn moving_state(entity: u64, instant: bool, refuse: bool, field: &'static str) -> (ClientState, Calls) {
    let calls = Calls::default();
    let mut state = ClientState::new(EntityId(entity), AccountId(entity), Pos::default(), AreaId { game: GameId::Ff7, key: FIELD.into() });
    state.scene.set_builder(Box::new(Builder { calls: Rc::clone(&calls), refuse, field }));
    state.scene.set_instant(instant);
    state.own.set_motion(Pos::new(10.0, 20.0, 30.0), 1.25, AnimState::Run);
    assert_eq!(state.own.next_intent(10.0).unwrap().seq, 1);
    assert!(state.own.on_rejected(1, state.own.pos()));
    state.own.set_motion(Pos::new(100.0, 200.0, 300.0), 2.0, AnimState::Run);
    let old = Pos::new(9.0, 19.0, 29.0);
    assert!(state.apply(10.01, ServerMsg::EntityUpdate { tick: 1, entities: vec![EntitySnapshot {
        id: EntityId(entity), pos: old, facing: 0.0, anim: AnimState::Idle,
        avatar: AvatarAppearance::new("Me", [1, 2, 3], None),
    }] }).is_none());
    assert_eq!(state.own.pos(), Pos::new(100.0, 200.0, 300.0), "the regular snapshot waits while prediction is fresh");
    assert_eq!(state.own.server_pos, Some(old));
    assert!(state.own.has_unsent());
    (state, calls)
}

fn pose(state: &ClientState) -> (Pos, f32, AnimState) {
    (state.own.pos(), state.own.facing(), state.own.anim())
}

#[test]
fn an_accepted_owner_origin_stops_fresh_prediction_exactly_without_echoing_or_resetting_the_sequence() {
    for instant in [false, true] {
        let (mut state, calls) = moving_state(1, instant, false, FIELD);
        let origin = origin();
        assert!(state.apply(10.02, start(SceneRole::Owner, Some(origin))).is_none());
        assert_eq!(*calls.borrow(), [Some(origin)]);
        assert_eq!(pose(&state), (origin.pos, origin.facing, AnimState::Idle));
        assert_eq!((state.own.seq, state.own.last_intent_t), (1, 10.0));
        assert_eq!(state.own.server_pos, None, "the older deferred snapshot cannot move the player back later");
        assert!(!state.own.resync && state.own.resync_seq.is_none() && !state.own.has_unsent());
        assert!(state.own.next_intent(10.03).is_none(), "the server already owns this exact pose");
        assert_eq!(state.scene.player().is_none(), instant, "even a source that finishes during construction adopts its accepted origin");
        if instant {
            assert_eq!(state.scene.take_commands(), [SceneCmd::End { run: RUN, skipped: false }]);
        }
        state.scene.end_local();
        state.own.set_motion(Pos::new(origin.pos.x + 5.0, origin.pos.y, origin.pos.z), 0.25, AnimState::Walk);
        assert!(state.own.next_intent(10.049).is_none());
        let next = state.own.next_intent(10.05).expect("the previous intent's clock survives the scene placement");
        assert_eq!((next.seq, next.pos, next.facing, next.anim), (2, state.own.pos(), 0.25, AnimState::Walk));
    }
}

#[test]
fn a_watcher_builds_from_the_owner_origin_without_adopting_it_or_clearing_its_own_prediction() {
    let (mut state, calls) = moving_state(2, false, false, FIELD);
    let before = pose(&state);
    let prediction = (state.own.last_sent, state.own.server_pos, state.own.resync, state.own.resync_seq);
    let origin = origin();
    assert!(state.apply(10.02, start(SceneRole::Watcher, Some(origin))).is_none());
    assert_eq!(*calls.borrow(), [Some(origin)]);
    assert!(state.scene.player().is_some());
    assert_eq!(pose(&state), before);
    assert_eq!((state.own.last_sent, state.own.server_pos, state.own.resync, state.own.resync_seq), prediction);
}

#[test]
fn missing_mixed_and_non_finite_origins_never_build_or_move_even_with_a_permissive_builder() {
    for invalid in ["missing", "mixed", "nan position", "infinite facing"] {
        let (mut state, calls) = moving_state(1, false, false, FIELD);
        let before = pose(&state);
        let prediction = (state.own.last_sent, state.own.server_pos, state.own.resync);
        let mut msg = start(SceneRole::Owner, Some(origin()));
        if let ServerMsg::SceneStart { presentation_origin, replay, .. } = &mut msg {
            match invalid {
                "missing" => *presentation_origin = None,
                "mixed" => *replay = Some(SceneReplay { source: [1; 32], origin: Some(origin()), events: vec![] }),
                "nan position" => presentation_origin.as_mut().unwrap().pos.z = f32::NAN,
                _ => presentation_origin.as_mut().unwrap().facing = f32::INFINITY,
            }
        }
        assert_eq!(state.apply(10.02, msg).is_some(), invalid == "mixed", "{invalid}");
        assert!(calls.borrow().is_empty(), "{invalid} cannot reach the builder");
        assert!(state.scene.player().is_none());
        assert_eq!(pose(&state), before, "{invalid}");
        assert_eq!((state.own.last_sent, state.own.server_pos, state.own.resync), prediction);
        assert_eq!(state.scene.take_commands(), [SceneCmd::Leave { run: RUN }]);
    }
}

#[test]
fn a_refused_source_wrong_field_or_unreadable_story_cannot_place_the_owner() {
    for failure in ["source", "field", "story"] {
        let (mut state, calls) = moving_state(1, false, failure == "source", if failure == "field" { "another-field" } else { FIELD });
        let before = pose(&state);
        let mut msg = start(SceneRole::Owner, Some(origin()));
        if failure == "story" {
            if let ServerMsg::SceneStart { progress, .. } = &mut msg { *progress = vec![255]; }
        }
        assert!(state.apply(10.02, msg).is_none());
        assert_eq!(calls.borrow().len(), usize::from(failure != "story"));
        assert!(state.scene.player().is_none());
        assert_eq!(pose(&state), before, "{failure}");
        assert!(state.own.has_unsent(), "{failure} cannot erase unsent prediction either");
        assert_eq!(state.scene.take_commands(), [SceneCmd::Leave { run: RUN }]);
    }
}

#[test]
fn an_ordinary_scene_without_an_origin_keeps_the_existing_prediction_behavior() {
    let (mut state, calls) = moving_state(1, false, false, FIELD);
    let before = pose(&state);
    let mut msg = start(SceneRole::Owner, None);
    if let ServerMsg::SceneStart { scene, .. } = &mut msg { *scene = "test/md1stin/ordinary".into(); }
    assert!(state.apply(10.02, msg).is_none());
    assert_eq!(*calls.borrow(), [None]);
    assert!(state.scene.player().is_some());
    assert_eq!(pose(&state), before);
    assert!(state.own.has_unsent());
}
