//! The ordinary story transaction's authoritative hold, independent of scene playback.

use super::*;
use fmv_adapter_api::GameAdapter;
use fmv_core::{GameId, PhaseKey};
use fmv_ff7::story::StoryState;
use fmv_fixture::{FixtureAdapter, FIXTURE_AREA_KEY};
use fmv_protocol::{InstanceKey, Placement};

const ID: u64 = 7;

fn area(key: &str) -> AreaId { AreaId { game: GameId::Fixture, key: key.into() } }

fn moving_state() -> ClientState {
    let mut state = ClientState::new(EntityId(1), AccountId(1), Pos::default(), area(FIXTURE_AREA_KEY));
    state.own.set_motion(Pos::new(10.0, 20.0, 30.0), 1.25, AnimState::Run);
    assert_eq!(state.own.next_intent(10.0).unwrap().seq, 1);
    state
}

fn pending(id: u64, pos: Pos, facing: f32) -> ServerMsg {
    ServerMsg::StoryPending { id, area: area(FIXTURE_AREA_KEY), pos, facing }
}

fn pose(state: &ClientState) -> (Pos, f32, AnimState) {
    (state.own.pos(), state.own.facing(), state.own.anim())
}

fn snapshot(area: AreaId, full: bool, pos: Pos, facing: f32) -> ServerMsg {
    let entities = vec![EntitySnapshot { id: EntityId(1), pos, facing, anim: AnimState::Idle,
        avatar: AvatarAppearance::new("Me", [1, 2, 3], None) }];
    if full { ServerMsg::AreaSnapshot { area, tick: 1, entities, npcs: vec![] } }
    else { ServerMsg::EntityUpdate { tick: 1, entities } }
}

#[test]
fn pending_installs_the_exact_idle_pose_and_blocks_all_prediction_and_rejection_paths() {
    let runtime = FixtureAdapter::new().load_area(FIXTURE_AREA_KEY).unwrap();
    for target in [Pos::new(10.0, 20.0, 30.25), Pos::new(10.25, 20.125, 30.25)] {
        let mut state = moving_state();
        let previous = state.own.pos();
        assert!(state.own.on_rejected(1, previous));
        assert!(state.own.has_unsent());
        assert!(state.apply(10.01, snapshot(area(FIXTURE_AREA_KEY), false, Pos::new(900.0, 800.0, 700.0), 2.0)).is_none());
        assert_eq!(state.own.pos(), previous, "an ordinary snapshot is deferred while prediction is fresh");
        let before = state.story_pending_epoch();
        assert!(state.apply(10.02, pending(ID, target, -0.75)).is_none());
        assert!(state.is_story_pending());
        assert_eq!(state.story_pending_id(), Some(ID));
        assert!(state.story_pending_epoch() > before);
        assert_eq!(pose(&state), (target, -0.75, AnimState::Idle));
        assert!(!state.own.has_unsent(), "the exact authoritative pose is already known to the server");
        assert_eq!(state.take_story_pending_ack(), Some(ID));
        assert_eq!(state.take_story_pending_ack(), None);

        state.own.set_motion(Pos::new(50.0, 60.0, 70.0), 0.5, AnimState::Run);
        assert!(!state.own.walk(runtime.as_ref(), (1.0, 0.0), 100.0, 1.0));
        assert!(!state.own.walk_as(runtime.as_ref(), (0.0, 1.0), 100.0, 1.0, AnimState::Run));
        assert!(!state.own.on_rejected(1, Pos::default()), "an old in-flight rejection cannot undo the installed hold");
        assert!(state.apply(10.03, ServerMsg::MoveRejected { seq: 1, corrected: Pos::new(-100.0, 0.0, 0.0) }).is_none());
        for now in [10.04, 10.1, 11.0] { assert!(state.own.next_intent(now).is_none()); }
        assert_eq!(pose(&state), (target, -0.75, AnimState::Idle));
        assert!(!state.own.has_unsent());

        assert!(state.apply(11.01, ServerMsg::StoryCommitted { id: ID }).is_none());
        assert!(!state.is_story_pending());
        assert_eq!(state.story_pending_id(), None);
        assert!(state.own.next_intent(12.0).is_none(), "neither the placement nor an obsolete deferred echo is resent after release");
        assert_eq!(pose(&state), (target, -0.75, AnimState::Idle));
        state.own.set_motion(Pos::new(target.x + 5.0, target.y, target.z), 0.25, AnimState::Walk);
        assert_eq!(state.own.next_intent(12.1).unwrap().seq, 2, "the hold preserves the movement sequence");
    }
}

#[test]
fn a_pending_and_commit_in_one_batch_preserve_the_rate_clock_and_leave_a_visible_epoch_change() {
    let mut state = moving_state();
    let before = state.story_pending_epoch();
    assert!(state.apply(10.02, pending(ID, Pos::new(10.0, 20.0, 30.25), -0.75)).is_none());
    assert!(state.apply(10.03, ServerMsg::StoryCommitted { id: ID }).is_none());
    assert!(!state.is_story_pending());
    assert!(state.story_pending_epoch() > before, "a renderer observing only the completed batch still clears keys held across the transaction");
    state.own.set_motion(Pos::new(11.0, 20.0, 30.25), 0.25, AnimState::Walk);
    assert!(state.own.next_intent(10.049).is_none());
    let next = state.own.next_intent(10.05).expect("the original intent at 10.0 determines the rate limit, not either hold message");
    assert_eq!((next.seq, next.pos, next.facing, next.anim), (2, Pos::new(11.0, 20.0, 30.25), 0.25, AnimState::Walk));
}

#[test]
fn exact_pending_retransmission_reacks_without_rewinding_the_authoritative_pose_or_epoch() {
    let mut state = moving_state();
    let original = Pos::new(20.0, 30.0, 40.0);
    assert!(state.apply(10.02, pending(ID, original, 0.75)).is_none());
    let epoch = state.story_pending_epoch();
    assert_eq!(state.take_story_pending_ack(), Some(ID));
    let final_pos = Pos::new(20.0, 30.0, 40.25);
    assert!(state.apply(10.03, snapshot(area(FIXTURE_AREA_KEY), true, final_pos, -0.75)).is_none());
    assert_eq!(pose(&state), (final_pos, -0.75, AnimState::Idle));
    assert!(state.apply(10.04, pending(ID, original, 0.75)).is_none());
    assert_eq!(state.take_story_pending_ack(), Some(ID));
    assert_eq!(state.story_pending_epoch(), epoch);
    assert_eq!(pose(&state), (final_pos, -0.75, AnimState::Idle), "the duplicate acknowledges the same hold; it does not repeat its initial placement");
    assert!(state.apply(10.05, pending(ID - 1, Pos::default(), 0.0)).is_none());
    assert!(state.apply(10.06, ServerMsg::StoryCommitted { id: ID - 1 }).is_none());
    assert_eq!(state.take_story_pending_ack(), None);
    assert_eq!(state.story_pending_id(), Some(ID));
    assert_eq!(state.story_pending_epoch(), epoch);

    assert!(state.apply(10.07, ServerMsg::StoryCommitted { id: ID }).is_none());
    assert!(state.apply(10.08, pending(ID, original, 0.75)).is_none());
    assert!(state.apply(10.09, ServerMsg::StoryCommitted { id: ID }).is_none());
    assert!(!state.is_story_pending());
    assert_eq!(state.take_story_pending_ack(), None, "a completed id is stale, not a new hold");
    assert_eq!(state.story_pending_epoch(), epoch);
    assert!(state.apply(10.1, pending(ID + 1, final_pos, -0.75)).is_none());
    assert_eq!(state.story_pending_id(), Some(ID + 1));
    assert!(state.story_pending_epoch() > epoch);
}

#[test]
fn conflicting_or_invalid_hold_messages_fail_closed_without_replacing_the_existing_hold() {
    for invalid in ["different position", "different heading", "overlap", "zero pending", "nan position", "infinite heading", "wrong area", "future commit", "zero commit"] {
        let mut state = moving_state();
        let original = Pos::new(20.0, 30.0, 40.0);
        assert!(state.apply(10.02, pending(ID, original, 0.75)).is_none());
        assert_eq!(state.take_story_pending_ack(), Some(ID));
        let epoch = state.story_pending_epoch();
        let message = match invalid {
            "different position" => pending(ID, Pos::new(20.0, 30.0, 40.25), 0.75),
            "different heading" => pending(ID, original, -0.75),
            "overlap" => pending(ID + 1, original, 0.75),
            "zero pending" => pending(0, original, 0.75),
            "nan position" => pending(ID, Pos::new(f32::NAN, 30.0, 40.0), 0.75),
            "infinite heading" => pending(ID, original, f32::INFINITY),
            "wrong area" => ServerMsg::StoryPending { id: ID, area: area("another-field"), pos: original, facing: 0.75 },
            "future commit" => ServerMsg::StoryCommitted { id: ID + 1 },
            _ => ServerMsg::StoryCommitted { id: 0 },
        };
        assert!(state.apply(10.03, message).is_some(), "{invalid} must end the session instead of changing or releasing the hold");
        assert_eq!(state.story_pending_id(), Some(ID));
        assert_eq!(state.story_pending_epoch(), epoch);
        assert_eq!(pose(&state), (original, 0.75, AnimState::Idle));
        assert_eq!(state.take_story_pending_ack(), None, "{invalid} must not be acknowledged");
        assert!(state.own.next_intent(11.0).is_none());
    }
}

#[test]
fn invalid_first_holds_and_unexpected_commits_are_not_acknowledged() {
    for invalid in ["zero pending", "nan position", "nan heading", "wrong area", "future commit", "zero commit"] {
        let mut state = moving_state();
        let original = pose(&state);
        let message = match invalid {
            "zero pending" => pending(0, Pos::default(), 0.0),
            "nan position" => pending(ID, Pos::new(0.0, 0.0, f32::NAN), 0.0),
            "nan heading" => pending(ID, Pos::default(), f32::NAN),
            "wrong area" => ServerMsg::StoryPending { id: ID, area: area("another-field"), pos: Pos::default(), facing: 0.0 },
            "future commit" => ServerMsg::StoryCommitted { id: ID },
            _ => ServerMsg::StoryCommitted { id: 0 },
        };
        assert!(state.apply(10.02, message).is_some(), "{invalid} is not a valid first transaction message");
        assert_eq!(state.take_story_pending_ack(), None);
        assert_eq!(pose(&state), original);
    }
}

#[test]
fn story_phase_and_exact_snapshots_do_not_release_the_pending_transaction() {
    for full_snapshot in [false, true] {
        let mut state = moving_state();
        assert!(state.apply(10.02, pending(ID, Pos::new(10.0, 20.0, 30.0), 0.75)).is_none());
        state.take_story_pending_ack();
        let epoch = state.story_pending_epoch();
        assert!(state.apply(10.03, ServerMsg::StoryUpdate { progress: StoryState::at(51).encode(), advance: None }).is_none());
        let placement = Placement { account: AccountId(1), key: InstanceKey::new(area(FIXTURE_AREA_KEY), PhaseKey(vec![51])),
            party: None, phase_owner: AccountId(1), story_advance_allowed: true };
        assert!(state.apply(10.04, ServerMsg::InstancePlacement { placement }).is_none());
        let target = Pos::new(10.0, 20.0, 30.25);
        assert!(state.apply(10.05, snapshot(area(FIXTURE_AREA_KEY), full_snapshot, target, -0.75)).is_none());
        assert_eq!(pose(&state), (target, -0.75, AnimState::Idle), "even a z-only change overrides recent prediction while held");
        assert_eq!(state.story_pending_id(), Some(ID));
        assert_eq!(state.story_pending_epoch(), epoch);
        assert!(state.own.next_intent(11.0).is_none());
        assert!(state.apply(11.01, ServerMsg::StoryCommitted { id: ID }).is_none());
        assert!(!state.is_story_pending());
        assert!(state.own.next_intent(12.0).is_none());
        assert_eq!(pose(&state), (target, -0.75, AnimState::Idle));
    }
}

#[test]
fn an_area_transfer_keeps_the_hold_until_its_matching_commit_including_same_field_reload() {
    for destination in [FIXTURE_AREA_KEY, "another-field"] {
        let mut state = moving_state();
        assert!(state.apply(10.02, pending(ID, Pos::new(10.0, 20.0, 30.0), 0.75)).is_none());
        state.take_story_pending_ack();
        let epoch = state.story_pending_epoch();
        state.change_area(area(destination), Pos::new(100.0, 200.0, 300.0), 0.5, true);
        assert_eq!(state.story_pending_id(), Some(ID));
        assert_eq!(state.story_pending_epoch(), epoch);
        let target = Pos::new(100.0, 200.0, 300.25);
        assert!(state.apply(10.03, snapshot(area(destination), true, target, -0.5)).is_none());
        assert_eq!(pose(&state), (target, -0.5, AnimState::Idle));
        state.own.set_motion(Pos::default(), 0.0, AnimState::Run);
        assert_eq!(pose(&state), (target, -0.5, AnimState::Idle));
        assert!(state.own.next_intent(11.0).is_none());
        assert!(state.apply(11.01, ServerMsg::StoryCommitted { id: ID }).is_none());
        assert!(!state.is_story_pending());
        assert!(state.own.next_intent(12.0).is_none());
        let walked = Pos::new(target.x + 1.0, target.y, target.z);
        state.own.set_motion(walked, 0.25, AnimState::Walk);
        assert_eq!(state.own.next_intent(12.1).unwrap().pos, walked);
    }
}

fn encounter(game_over_on_defeat: bool) -> ServerMsg {
    ServerMsg::EncounterStart {
        battle_id: fmv_protocol::BattleId(42), formation_id: 1, seed: 7,
        party: vec![fmv_protocol::BattleMember::player(EntityId(1), AccountId(1), "Me", 0, 6, 0)],
        initial_party: None, escape_policy: fmv_protocol::EscapePolicy::Formation,
        victory_fanfare: true, game_over_on_defeat, preemptive: false, back_attack: false,
        battle_speed: 0, ticks_per_second: 1000, data_fingerprint: fmv_protocol::SyntheticRules::FINGERPRINT,
    }
}

fn defeat() -> ServerMsg {
    ServerMsg::BattleEnd { battle_id: fmv_protocol::BattleId(42), outcome: fmv_protocol::battle::Outcome::Defeat, rewards: vec![] }
}

#[test]
fn ordinary_game_over_waits_for_the_authoritative_end_and_outlives_the_result_panel() {
    let runtime = FixtureAdapter::new().load_area(FIXTURE_AREA_KEY).unwrap();
    for terminal in [false, true] {
        let mut state = moving_state();
        state.battle.set_rules(None);
        assert!(state.apply(10.01, encounter(terminal)).is_none());
        assert!(state.apply(10.02, ServerMsg::BattleEvent { battle_id: fmv_protocol::BattleId(42), tick: 1,
            events: vec![fmv_protocol::battle::BattleEvent::Defeat], command: None }).is_none());
        assert!(!state.ordinary_game_over(), "an outcome event is not the acknowledged end");
        assert!(state.battle.in_battle());
        assert!(state.apply(10.03, ServerMsg::BattleEnd { battle_id: fmv_protocol::BattleId(99),
            outcome: fmv_protocol::battle::Outcome::Defeat, rewards: vec![] }).is_none());
        assert!(!state.ordinary_game_over(), "another battle's end cannot install a terminal hold");
        assert!(state.apply(11.0, defeat()).is_none());
        assert_eq!(state.ordinary_game_over(), terminal);
        assert_eq!(state.field_controls_held(), terminal);
        assert!(!state.is_story_pending(), "Game Over does not invent a story transaction token");
        assert!(!state.battle.lingering(1000.0));
        let before = pose(&state);
        state.own.set_motion(Pos::new(20.0, 30.0, 40.0), 0.5, AnimState::Run);
        if terminal {
            assert_eq!(pose(&state), before);
            assert!(!state.own.walk(runtime.as_ref(), (1.0, 0.0), 100.0, 1.0));
            assert!(!state.own.walk_as(runtime.as_ref(), (0.0, 1.0), 100.0, 1.0, AnimState::Run));
            assert!(state.apply(1000.0, ServerMsg::MoveRejected { seq: 1, corrected: Pos::default() }).is_none());
            assert_eq!(pose(&state), before, "late rejections cannot resume field prediction");
            assert!(state.own.next_intent(1000.1).is_none());
        } else {
            assert_eq!(state.own.next_intent(1000.1).unwrap().pos, Pos::new(20.0, 30.0, 40.0),
                "a debug defeat preserves its explicit return-to-field policy");
        }
    }
}

#[test]
fn a_defeat_never_releases_the_pending_token_and_only_nonterminal_defeats_can_commit_it() {
    for terminal in [false, true] {
        let mut state = moving_state();
        state.battle.set_rules(None);
        assert!(state.apply(10.01, pending(ID, Pos::new(10.0, 20.0, 30.0), 0.75)).is_none());
        state.take_story_pending_ack();
        let epoch = state.story_pending_epoch();
        assert!(state.apply(10.02, encounter(terminal)).is_none());
        assert!(state.apply(11.0, defeat()).is_none());
        assert_eq!(state.story_pending_id(), Some(ID));
        assert_eq!(state.story_pending_epoch(), epoch);
        assert!(state.field_controls_held());
        assert!(state.own.next_intent(1000.0).is_none());
        let result = state.apply(1000.1, ServerMsg::StoryCommitted { id: ID });
        assert_eq!(result.is_some(), terminal, "an ordinary authored defeat has no release on this connection");
        assert_eq!(state.story_pending_id(), terminal.then_some(ID));
        assert_eq!(state.field_controls_held(), terminal);
        assert_eq!(state.story_pending_epoch(), epoch);
        assert_eq!(state.take_story_pending_ack(), None);
    }
}

#[test]
fn unexpected_encounters_cannot_reset_an_ordinary_terminal_defeat() {
    let mut state = moving_state();
    state.battle.set_rules(None);
    assert!(state.apply(10.01, encounter(true)).is_none());
    assert!(state.apply(11.0, defeat()).is_none());
    for terminal in [false, true] {
        assert!(state.apply(1000.0, encounter(terminal)).is_some());
        assert_eq!(state.battle.game_over(), Some(fmv_protocol::BattleId(42)));
        assert!(!state.battle.in_battle());
        assert_eq!(state.battle.entered(), 1);
        assert!(state.field_controls_held());
        assert!(state.own.next_intent(1000.1).is_none());
    }
}

#[test]
fn area_replacement_and_authoritative_snapshots_preserve_the_terminal_prediction_hold() {
    let mut state = moving_state();
    state.battle.set_rules(None);
    assert!(state.apply(10.01, encounter(true)).is_none());
    assert!(state.apply(11.0, defeat()).is_none());
    let destination = area("another-field");
    state.change_area(destination.clone(), Pos::new(100.0, 200.0, 300.0), 0.5, true);
    let target = Pos::new(100.0, 200.0, 300.25);
    assert!(state.apply(1000.0, snapshot(destination, true, target, -0.5)).is_none());
    assert!(state.apply(1000.1, ServerMsg::StoryUpdate { progress: StoryState::at(51).encode(), advance: None }).is_none());
    assert!(state.ordinary_game_over() && state.field_controls_held());
    assert_eq!(pose(&state), (target, -0.5, AnimState::Idle));
    state.own.set_motion(Pos::default(), 0.0, AnimState::Run);
    assert_eq!(pose(&state), (target, -0.5, AnimState::Idle));
    assert!(state.own.next_intent(1000.2).is_none());
}

#[test]
fn a_later_scene_cannot_reclassify_an_acknowledged_ordinary_defeat() {
    let mut state = moving_state();
    state.battle.set_rules(None);
    assert!(state.apply(10.01, pending(ID, Pos::new(10.0, 20.0, 30.0), 0.75)).is_none());
    assert!(state.apply(10.02, encounter(true)).is_none());
    assert!(state.apply(11.0, defeat()).is_none());
    let scene_start = ServerMsg::SceneStart { presentation_origin: None,
        run: fmv_protocol::SceneRunId(12), scene: "test/late/scene".into(), seed: 1,
        progress: StoryState::at(39).encode(), gil: 0, replay: None,
        owner: EntityId(1), participants: vec![], you: fmv_protocol::SceneRole::Owner,
    };
    assert!(state.apply(11.01, scene_start).is_some_and(|reason| reason.contains("Game Over")));
    assert!(!state.scene.is_active(), "the invalid replacement never reaches scene construction");
    for message in [
        ServerMsg::SceneStep { run: fmv_protocol::SceneRunId(12), step: 0, answer: None },
        ServerMsg::SceneBattleStarted { run: fmv_protocol::SceneRunId(12), ordinal: 1, battle_id: fmv_protocol::BattleId(43) },
    ] {
        let _ = state.apply(11.02, message);
        assert!(state.ordinary_game_over() && state.field_controls_held());
    }
    state.scene.end_local();
    assert!(state.ordinary_game_over(), "scene teardown cannot change the classification either");
    assert!(state.apply(11.03, ServerMsg::StoryCommitted { id: ID }).is_some());
    assert!(state.apply(11.04, encounter(false)).is_some());
    assert_eq!(state.story_pending_id(), Some(ID));
    assert_eq!(state.battle.entered(), 1);
    assert!(state.own.next_intent(1000.0).is_none());
}
