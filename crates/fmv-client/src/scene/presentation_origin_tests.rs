//! An ordinary origin is a source input, without opting into durable replay.
use super::*;
use fmv_protocol::{SceneOrigin, SceneReplay};

const STATION: &str = "ff7/md1stin/station-guards";

type Captured = Rc<RefCell<Vec<SceneOrigin>>>;
struct OriginBuilder { captured: Captured }
impl SceneBuilder for OriginBuilder {
    fn build(&mut self, _: &str, _: &fmv_ff7::story::StoryState, _: u32, _: SceneResources) -> Result<Built, String> {
        panic!("an origin-dependent presentation cannot use a canned/private source")
    }
    fn build_with_origin(&mut self, scene: &str, story: &fmv_ff7::story::StoryState, seed: u32,
        resources: SceneResources, origin: Option<&SceneOrigin>) -> Result<Built, String> {
        assert_eq!((scene, story.moment, seed, resources.gil), (STATION, 1, 5, Some(7)));
        let origin = *origin.expect("the explicit accepted pose reaches the builder");
        origin.validate().map_err(str::to_string)?;
        self.captured.borrow_mut().push(origin);
        Ok(Built { def: SceneDef { id: STATION.into(), witness: vec![], ..def() },
            source: Box::new(Scripted { initial: vec![], beats: talk().into(), asked: Asked::default() }) })
    }
    fn reach(&mut self, _: usize, _: &str) -> Option<f32> { None }
}

fn message(role: SceneRole, origin: Option<SceneOrigin>, replay: Option<SceneReplay>) -> ServerMsg {
    ServerMsg::SceneStart {
        run: RUN, scene: STATION.into(), seed: 5,
        progress: fmv_ff7::story::StoryState::fresh_first_control().encode(), gil: 7,
        replay, presentation_origin: origin, owner: EntityId(1),
        participants: vec![SceneParticipant { entity: EntityId(1), spot: 0 },
            SceneParticipant { entity: EntityId(2), spot: 1 }], you: role,
    }
}

#[test]
fn ordinary_owner_and_watcher_bind_the_same_exact_origin_without_durable_replay() {
    let origin = SceneOrigin { pos: Pos::new(3783.25, 28446.5, 310.0), facing: 1.25, triangle: Some(12) };
    for role in [SceneRole::Owner, SceneRole::Watcher] {
        let captured = Captured::default();
        let mut client = SceneClient::new();
        client.set_builder(Box::new(OriginBuilder { captured: Rc::clone(&captured) }));
        client.on_message(AREA, message(role, Some(origin), None));
        assert_eq!(*captured.borrow(), [origin]);
        let player = client.player().expect("ordinary source was built");
        assert!(!player.is_durable(), "an origin is not a saved continuation prefix");
        assert!(player.witnesses().is_empty(), "the source does not substitute a witness position");
        assert!(client.failure().is_none());
    }
}

#[test]
fn a_missing_ordinary_origin_or_two_origin_channels_never_build_a_source() {
    let origin = SceneOrigin { pos: Pos::new(3783.0, 28446.0, 310.0), facing: 0.0, triangle: Some(12) };
    for (ordinary, replay, reserved) in [
        (None, None, false),
        (Some(origin), Some(SceneReplay { source: [1; 32], origin: Some(origin), events: vec![] }), true),
    ] {
        let captured = Captured::default();
        let mut client = SceneClient::new();
        client.set_builder(Box::new(OriginBuilder { captured: Rc::clone(&captured) }));
        client.on_message(AREA, message(SceneRole::Owner, ordinary, replay));
        assert!(captured.borrow().is_empty());
        assert!(client.player().is_none());
        assert_eq!(client.failure().is_some(), reserved);
        assert_eq!(client.take_commands(), [SceneCmd::Leave { run: RUN }]);
    }
}

#[test]
fn a_malformed_replacement_does_not_leave_an_older_ordinary_scene_visible() {
    let mut rig = rig_with(talk(), false);
    rig.client.on_message(AREA, start(RUN, SceneRole::Owner, &[(1, 0)]));
    assert!(rig.client.player().is_some());
    let mut malformed = message(SceneRole::Owner, None, None);
    if let ServerMsg::SceneStart { run, .. } = &mut malformed { *run = SceneRunId(RUN.0 + 1); }
    rig.client.on_message(AREA, malformed);
    assert!(rig.client.player().is_none());
    assert_eq!(rig.client.take_commands(), [SceneCmd::Leave { run: SceneRunId(RUN.0 + 1) }]);
}
