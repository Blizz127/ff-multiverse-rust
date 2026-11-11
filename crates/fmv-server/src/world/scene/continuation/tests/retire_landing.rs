//! Retiring a terminal landing is a durable metadata transition, not an area-handshake ACK.

use super::*;

const TERMINAL: &str = "fixture/terminal-travel";
const TERMINAL_SOURCE: [u8; 32] = [81; 32];
const REQUIRED: &str = "fixture/unavailable-required-arrival";
const LEGACY: &str = "fixture/ordinary-arrival";

#[derive(Clone, Copy, Debug, Default)]
enum Variant {
    #[default]
    Normal,
    WrongPosition,
    WrongFacing,
    WrongHint,
    ChangedSource,
    Incomplete,
    RequiredUnavailable,
    LegacyArrival,
}

struct TerminalHost(Variant);

impl SceneHost for TerminalHost {
    fn scenes(&self) -> Vec<SceneInfo> {
        let mut scenes = vec![SceneInfo { id: TERMINAL.into(), field: HALL.into(), arrival: Pos::default(),
            arrival_facing: 0.0, arrival_hint: None }];
        let next = match self.0 { Variant::RequiredUnavailable => Some(REQUIRED), Variant::LegacyArrival => Some(LEGACY), _ => None };
        if let Some(id) = next {
            scenes.push(SceneInfo { id: id.into(), field: YARD.into(), arrival: DoorHost::landing().arrival,
                arrival_facing: 1.5, arrival_hint: Some(29) });
        }
        scenes
    }
    fn offered(&self, scene: &str, progress: &[u8]) -> bool {
        (scene == TERMINAL && progress == DOOR_STORY) || ((scene == REQUIRED || scene == LEGACY) && progress == ARRIVED_STORY)
    }
    fn automatic(&self, scene: &str) -> bool { scene == REQUIRED || scene == LEGACY }
    fn requires_landing(&self, scene: &str) -> bool { scene == REQUIRED }
    fn continuation(&self, scene: &str) -> bool { scene == TERMINAL }
    fn noncombat_continuation(&self, scene: &str) -> bool { scene == TERMINAL }
    fn trigger(&self, _: &str, _: &[u8]) -> Option<TriggerInfo> { None }
    fn preview_progress(&self, _: &str) -> Option<Vec<u8>> { None }
    fn entry(&self, progress: &[u8]) -> Option<Exit> {
        if progress != ARRIVED_STORY { return None; }
        let mut entry = DoorHost::landing();
        match self.0 {
            Variant::WrongPosition => entry.arrival.x += 1.0,
            Variant::WrongFacing => entry.arrival_facing = Some(0.5),
            Variant::WrongHint => entry.arrival_hint = Some(28),
            _ => {},
        }
        Some(entry)
    }
    fn finish(&self, scene: &str, progress: &[u8], _: u32, _: &[u8]) -> Option<SceneResult> {
        (scene == LEGACY).then(|| SceneResult { progress: progress.to_vec(), changed: false,
            summary: "ordinary arrival".into(), travel: None, transaction: None })
    }
    fn evaluate_continuation(&self, scene: &str, original: &[u8], _: u32, gil: u32, replay: Option<&SceneReplay>) -> Option<SceneBoundary> {
        let source = if matches!(self.0, Variant::ChangedSource) { [82; 32] } else { TERMINAL_SOURCE };
        if scene != TERMINAL || original != DOOR_STORY || replay.is_some_and(|r| r.source != source || r.validate().is_err()) { return None; }
        let events = replay.map_or(&[][..], |r| r.events.as_slice());
        let (progress, at) = match events {
            [] => (DOOR_STORY, SceneBoundaryKind::Window { step: 0, identity: vec![81], choices: None }),
            [SceneReplayEvent::Window { step: 0, identity, answer: None }] if identity == &[81] => {
                if matches!(self.0, Variant::Incomplete) {
                    (DOOR_STORY, SceneBoundaryKind::Window { step: 1, identity: vec![82], choices: None })
                } else {
                    (ARRIVED_STORY, SceneBoundaryKind::Complete(SceneResult { progress: ARRIVED_STORY.to_vec(), changed: true,
                        summary: "terminal travel".into(), travel: Some(DoorHost::landing()),
                        transaction: Some(SceneTransaction { expected_gil: gil, next_gil: gil }) }))
                }
            },
            _ => return None,
        };
        Some(SceneBoundary { source, progress: progress.to_vec(), gil, at })
    }
}

fn terminal_world(rules: SyntheticRules, home: &str, variant: Variant) -> (World, mpsc::UnboundedReceiver<Msg>) {
    let (mut world, saves) = world_at(rules, 3, home);
    world.configure_scenes(Some(SceneSettings { host: Arc::new(TerminalHost(variant)), debug_preview: false, debug_new_story: None }));
    (world, saves)
}

fn terminal_rig() -> Rig {
    let mut rig = door_rig(YARD);
    rig.send(ClientMsg::SceneEnd { run: rig.run, skipped: true });
    rig.owner.drain();
    assert!(rig.world.scenes.continuations.is_empty());
    rig.world.configure_scenes(Some(SceneSettings { host: Arc::new(TerminalHost(Variant::Normal)), debug_preview: false, debug_new_story: None }));
    start_terminal(&mut rig, vec![]);
    rig
}

fn start_terminal(rig: &mut Rig, watchers: Vec<EntityId>) {
    rig.world.start_run(rig.run, SceneRun { scene: TERMINAL.into(), owner: rig.owner.entity, watchers, seed: 19,
        progress: DOOR_STORY.to_vec(), gil: 240, answers: vec![], steps: 0, started: rig.now, last_activity: rig.now,
        writes_story: true, preview: false, then: None }, &mut vec![]);
    assert!(rig.owner.drain().iter().any(|m| matches!(m, ServerMsg::SceneStart { scene, replay: Some(_), .. } if scene == TERMINAL)));
}

fn land(rig: &mut Rig) -> SceneState {
    rig.send(ClientMsg::SceneStep { run: rig.run, step: 0, answer: None });
    rig.send(ClientMsg::SceneEnd { run: rig.run, skipped: false });
    rig.snapshot();
    let (job, done) = rig.pending_batch();
    assert_eq!(job.operation.kind, SceneOperationKind::Finish);
    let saved = rig.apply(&job);
    rig.ack(done, saved);
    let state = rig.stored();
    assert!(matches!(state.checkpoint.as_ref().unwrap().phase, ScenePhase::Landing { .. }));
    assert_eq!(rig.world.players[&rig.owner.entity].scene, Some(rig.run));
    state
}

fn replace_host(rig: &mut Rig, variant: Variant) {
    let host = Arc::new(TerminalHost(variant));
    rig.world.scenes.list = host.scenes();
    rig.world.scenes.host = Some(host);
}

fn restart_terminal(rig: Rig) -> Rig {
    let Rig { _dir, store, world, saves, rules, formation, owner, now, .. } = rig;
    let account = owner.account;
    let root = store.root().to_path_buf();
    drop((world, saves, owner, store));
    let store = Store::open(root).unwrap();
    let group = store.get_scene_group(account, GameId::Fixture).unwrap();
    let state = group[0].clone();
    let (mut world, saves) = terminal_world(rules, HALL, Variant::Normal);
    let owner = join(&mut world, state, group, now);
    let run = world.players[&owner.entity].scene.expect("retained landing holds its recovered owner");
    Rig { _dir, store, world, saves, rules, formation, owner, now, run }
}

fn retirement(rig: &Rig) -> Option<Landing> {
    rig.world.retirable_continuation_landing(&rig.world.scenes.continuations[&rig.run], &rig.world.scenes.runs[&rig.run])
}

#[test]
fn terminal_landing_survives_restart_and_releases_only_after_exact_metadata_ack() {
    let mut rig = terminal_rig();
    let landed = land(&mut rig);
    let mut rig = restart_terminal(rig);
    assert_eq!(rig.stored(), landed);
    assert!(rig.saves.try_recv().is_err(), "admission has not acknowledged the destination handshake");
    enter_door_destination(&mut rig);
    let (job, done) = rig.pending_batch();
    assert_eq!(job.operation.kind, SceneOperationKind::Complete);
    assert_eq!(job.members.len(), 1);
    let member = &job.members[0];
    assert_eq!(member.expected, landed);
    assert!(member.next_story.is_none() && member.next_gil.is_none() && member.battle.is_none());
    assert!(member.checkpoint.is_none() && member.reservation.is_none());
    assert_eq!(rig.stored(), landed, "entering the destination alone cannot delete recovery metadata");
    assert!(!rig.owner.drain().iter().any(|m| matches!(m, ServerMsg::SceneEnded { .. })));
    let saved = rig.apply(&job);
    assert_eq!(saved.current[0].pair, landed.pair, "every story/character field and timestamp remains byte-for-byte logical data");
    assert!(rig.stored().checkpoint.is_none());
    assert_eq!(rig.world.players[&rig.owner.entity].scene, Some(rig.run), "disk commit is not a runtime acknowledgement");
    let duplicate = rig.apply(&job);
    assert_eq!(duplicate.commit, fmv_persist::SceneCommit::AlreadyApplied);
    assert_eq!(duplicate.current, saved.current, "a repeated metadata operation cannot replay travel or mutate progress");
    rig.ack(done, saved);
    assert_eq!(rig.world.players[&rig.owner.entity].scene, None);
    assert_eq!(rig.world.players[&rig.owner.entity].area, YARD);
    assert!(rig.owner.drain().iter().any(|m| matches!(m, ServerMsg::SceneEnded { outcome: SceneOutcome::Finished, .. })));
    assert_eq!(rig.stored().pair, landed.pair);
    assert!(rig.saves.try_recv().is_err());
}

#[test]
fn terminal_retirement_waits_for_connected_followers_and_releases_them_after_the_same_ack() {
    let mut rig = terminal_rig();
    rig.send(ClientMsg::SceneEnd { run: rig.run, skipped: true });
    rig.owner.drain();
    let (account, _) = rig.store.create_account("Terminal follower", 2).unwrap();
    rig.store.put_progression(&Progression::new(account, GameId::Fixture, DOOR_STORY.to_vec(), 2)).unwrap();
    let mut character = progress::fresh(&rig.rules, 1).to_record(account, GameId::Fixture, 2);
    character.gil = 79;
    character.inventory = Some(vec![InventoryEntry::new(0, 3)]);
    rig.store.put_character_progress(&character).unwrap();
    let guest_before = rig.store.get_scene_state(account, GameId::Fixture).unwrap();
    let mut guest = join(&mut rig.world, guest_before.clone(), vec![guest_before.clone()], rig.now);
    assert_eq!(rig.world.party_invite(rig.owner.entity, guest.entity, rig.now), crate::world::PartyOutcome::Done);
    let invite = rig.world.parties.invites().next().unwrap().id;
    assert_eq!(rig.world.party_accept(guest.entity, invite, rig.now), crate::world::PartyOutcome::Done);
    start_terminal(&mut rig, vec![guest.entity]);
    guest.drain();
    let landed = land(&mut rig);
    let guest_offer = guest.drain().into_iter().find(|m| matches!(m, ServerMsg::AreaChange { .. })).unwrap();
    enter_door_destination(&mut rig);
    assert!(rig.saves.try_recv().is_err(), "the owner waits for its connected follower's handshake");
    let ServerMsg::AreaChange { area, fingerprint, provenance, .. } = guest_offer else { unreachable!() };
    assert_eq!(rig.world.join_area(guest.entity, &area, &fingerprint, &provenance, rig.now), crate::world::TravelReply::Done);
    let (job, done) = rig.pending_batch();
    assert_eq!(job.operation.kind, SceneOperationKind::Complete);
    assert_eq!(job.members.len(), 1, "the presentation follower acquires no durable scene write");
    assert_eq!(job.members[0].expected.pair.story.account_id, rig.owner.account);
    for entity in [rig.owner.entity, guest.entity] { assert_eq!(rig.world.players[&entity].scene, Some(rig.run)); }
    for messages in [rig.owner.drain(), guest.drain()] {
        assert!(!messages.iter().any(|m| matches!(m, ServerMsg::SceneEnded { .. })));
    }
    let saved = rig.apply(&job);
    for entity in [rig.owner.entity, guest.entity] { assert_eq!(rig.world.players[&entity].scene, Some(rig.run)); }
    rig.ack(done, saved);
    for (entity, messages) in [(rig.owner.entity, rig.owner.drain()), (guest.entity, guest.drain())] {
        assert_eq!(rig.world.players[&entity].scene, None);
        assert_eq!(rig.world.players[&entity].area, YARD);
        assert!(messages.iter().any(|m| matches!(m, ServerMsg::SceneEnded { outcome: SceneOutcome::Finished, .. })));
    }
    assert_eq!(rig.stored().pair, landed.pair);
    assert_eq!(rig.store.get_scene_state(account, GameId::Fixture).unwrap(), guest_before);
    assert_eq!(rig.world.players[&guest.entity].progression, DOOR_STORY);
    assert!(rig.saves.try_recv().is_err());
}

#[test]
fn changed_login_placement_or_unavailable_required_arrival_cannot_retire_landing() {
    for variant in [Variant::WrongPosition, Variant::WrongFacing, Variant::WrongHint, Variant::RequiredUnavailable] {
        let mut rig = terminal_rig();
        let landed = land(&mut rig);
        replace_host(&mut rig, variant);
        assert!(retirement(&rig).is_none(), "{variant:?}");
        if matches!(variant, Variant::RequiredUnavailable) {
            let host = rig.world.scenes.host.as_ref().unwrap();
            assert!(host.offered(REQUIRED, ARRIVED_STORY) && host.requires_landing(REQUIRED));
            assert!(!host.continuation_for(REQUIRED, ARRIVED_STORY), "unsupported arrivals still prevent retirement");
        }
        enter_door_destination(&mut rig);
        assert!(rig.saves.try_recv().is_err(), "{variant:?}: no metadata retirement or replacement is queued");
        assert_eq!(rig.stored(), landed, "{variant:?}: the durable recovery point is retained");
        assert!(rig.world.scenes.storage_failed, "{variant:?}: an unverifiable landing cannot return control");
        assert!(!rig.owner.drain().iter().any(|m| matches!(m, ServerMsg::SceneEnded { outcome: SceneOutcome::Finished, .. })));
    }
}

#[test]
fn changed_source_or_incomplete_transcript_cannot_recover_as_a_terminal_landing() {
    for variant in [Variant::ChangedSource, Variant::Incomplete] {
        let mut rig = terminal_rig();
        let landed = land(&mut rig);
        replace_host(&mut rig, variant);
        assert!(retirement(&rig).is_none(), "{variant:?}");
        let (mut world, mut saves) = terminal_world(rig.rules, HALL, variant);
        assert!(matches!(admit(&mut world, landed.clone(), vec![landed.clone()], rig.now), Err(crate::world::JoinError::StorageUnavailable)), "{variant:?}");
        assert!(!world.has_session(rig.owner.account));
        assert!(saves.try_recv().is_err());
        assert_eq!(rig.stored(), landed);
    }
    // An actually truncated durable transcript must fail under the unchanged interpreter too.
    let mut rig = terminal_rig();
    let landed = land(&mut rig);
    let mut truncated = landed.clone();
    let checkpoint = truncated.checkpoint.as_mut().unwrap();
    let mut payload: Payload = serde_json::from_slice(&checkpoint.replay).unwrap();
    payload.replay.events.clear();
    checkpoint.replay = serde_json::to_vec(&payload).unwrap();
    let (mut world, mut saves) = terminal_world(rig.rules, HALL, Variant::Normal);
    assert!(matches!(admit(&mut world, truncated.clone(), vec![truncated], rig.now), Err(crate::world::JoinError::StorageUnavailable)));
    assert!(!world.has_session(rig.owner.account));
    assert!(saves.try_recv().is_err());
    assert_eq!(rig.stored(), landed);
}

#[test]
fn retirement_requires_the_interpreted_story_and_gil_to_match_the_saved_pair() {
    for change_story in [true, false] {
        let mut rig = terminal_rig();
        let landed = land(&mut rig);
        let c = rig.world.scenes.continuations.get_mut(&rig.run).unwrap();
        if change_story { c.states[0].pair.story.blob.push(99); }
        else { c.states[0].pair.character.as_mut().unwrap().gil += 1; }
        assert!(retirement(&rig).is_none(), "a stale interpreted boundary cannot erase newer state");
        assert_eq!(rig.stored(), landed);
    }
}

#[test]
fn a_noncombat_landing_cannot_import_battle_plan_or_carried_hp() {
    for with_plan in [false, true] {
        let mut rig = terminal_rig();
        let plan = rig.world.prepare_scene_battle(rig.owner.entity, rig.run, 3, EscapePolicy::Forbidden, None, None).unwrap();
        let landed = land(&mut rig);
        let c = rig.world.scenes.continuations.get_mut(&rig.run).unwrap();
        if with_plan { c.payload.plan = Some(plan); }
        else { c.payload.carry = Some(SceneCombatCarry::default()); }
        let mut forged = landed.clone();
        forged.checkpoint.as_mut().unwrap().replay = serde_json::to_vec(&c.payload).unwrap();
        assert!(!rig.world.continuation_carry_allowed(TERMINAL, &rig.world.scenes.continuations[&rig.run].payload));
        assert!(retirement(&rig).is_none());
        let (mut world, mut saves) = terminal_world(rig.rules, HALL, Variant::Normal);
        assert!(matches!(admit(&mut world, forged.clone(), vec![forged], rig.now), Err(crate::world::JoinError::StorageUnavailable)));
        assert!(saves.try_recv().is_err());
        assert_eq!(rig.stored(), landed);
    }
}

#[test]
fn an_ordinary_new_scene_starts_after_retirement_without_becoming_a_durable_arrival() {
    let mut rig = terminal_rig();
    let landed = land(&mut rig);
    replace_host(&mut rig, Variant::LegacyArrival);
    enter_door_destination(&mut rig);
    let (job, done) = rig.pending_batch();
    assert_eq!(job.operation.kind, SceneOperationKind::Complete, "the ordinary arrival does not divert the landing into ContinueLanding");
    assert!(!rig.owner.drain().iter().any(|m| matches!(m, ServerMsg::SceneStart { scene, .. } if scene == LEGACY)));
    let saved = rig.apply(&job);
    rig.ack(done, saved);
    let messages = rig.owner.drain();
    let released = messages.iter().position(|m| matches!(m, ServerMsg::SceneEnded { outcome: SceneOutcome::Finished, .. })).unwrap();
    let started = messages.iter().position(|m| matches!(m, ServerMsg::SceneStart { scene, replay: None, .. } if scene == LEGACY)).unwrap();
    assert!(released < started, "the legacy scene begins only after the durable hold was released");
    assert!(rig.world.scenes.continuations.is_empty());
    assert_eq!(rig.stored().pair, landed.pair);
    assert!(rig.stored().checkpoint.is_none());
    assert!(rig.saves.try_recv().is_err());
}
