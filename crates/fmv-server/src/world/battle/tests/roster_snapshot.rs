//! Ordinary story battles use the intermediate roster from one authored action, while
//! their durable predecessor remains the player's original story until victory commits.

use super::*;
use std::sync::{atomic::AtomicUsize, Mutex};
use fmv_adapter_api::{AdapterError, StoryBattleAction, StoryBattlePolicy, StoryStep};
use fmv_ff7::story::{checked_canon_party_of, StoryState};

#[derive(Default)]
struct Calls {
    actions: AtomicUsize,
    checked_parties: Mutex<Vec<Vec<u8>>>,
}

struct SnapshotArea {
    inner: Box<dyn AreaRuntime>,
    before: Vec<u8>,
    action: StoryBattleAction,
    calls: Arc<Calls>,
}

impl AreaRuntime for SnapshotArea {
    fn info(&self) -> &AreaInfo { self.inner.info() }
    fn spawn_points(&self) -> Vec<Pos> { self.inner.spawn_points() }
    fn validate_move(&self, from: Pos, to: Pos) -> MoveResult { self.inner.validate_move(from, to) }
    fn bounds(&self) -> Bounds { self.inner.bounds() }
    fn walk_triangles(&self) -> Vec<[Pos; 3]> { self.inner.walk_triangles() }
    fn max_speed(&self) -> f32 { self.inner.max_speed() }
    // This is an explicitly synthetic Fixture area, not FF7 runtime admission of v4.
    fn supports_saved_progression(&self, _: &[u8]) -> bool { true }
    fn story_requires_transaction(&self, _: &[u8]) -> bool { true }
    fn story_battle_action(&self, progress: &[u8], point: u32) -> Option<StoryBattleAction> {
        assert_eq!(point, 7);
        assert_eq!(progress, self.before);
        self.calls.actions.fetch_add(1, Ordering::SeqCst);
        Some(self.action.clone())
    }
    fn story_battle_policy(&self, _: &[u8], _: u32) -> Option<StoryBattlePolicy> {
        panic!("the combined authored action must not be reevaluated for policy")
    }
    fn story_advance(&self, _: &[u8], _: u32) -> Option<StoryStep> {
        panic!("the combined authored action must not be reevaluated for its victory")
    }
    fn story_party(&self, _: &[u8]) -> Vec<u8> {
        panic!("unknown native party membership must not become a guessed roster")
    }
    fn try_story_party(&self, progress: &[u8]) -> Result<Vec<u8>, AdapterError> {
        self.calls.checked_parties.lock().unwrap().push(progress.to_vec());
        checked_canon_party_of(progress).ok_or_else(|| AdapterError::Unsupported("unknown fixture party".into()))
    }
}

fn native_story(slots: Option<[u8; 3]>) -> Vec<u8> {
    let mut story = StoryState::fresh_first_control().into_native_party().unwrap();
    if let Some(slots) = slots { story.native_party_mut().unwrap().write_main_slots(slots, 7).unwrap(); }
    story.encode()
}

fn snapshot_world(rules: SyntheticRules, before: &[u8], battle_progress: Option<Vec<u8>>, victory: &[u8])
    -> (World, mpsc::UnboundedReceiver<Msg>, Arc<Calls>) {
    let calls = Arc::new(Calls::default());
    let area = SnapshotArea {
        inner: FixtureAdapter::new().load_area(FIXTURE_AREA_KEY).unwrap(),
        before: before.to_vec(), calls: Arc::clone(&calls),
        action: StoryBattleAction {
            policy: StoryBattlePolicy { formation: 3, escape_forbidden: true,
                victory_fanfare: false, game_over_on_defeat: false },
            battle_progress,
            victory: StoryStep { progress: victory.to_vec(), summary: "the fixture action won".into(), countdown: None },
        },
    };
    let mut world = World::new(Box::new(area)).unwrap();
    world.configure_battle(Some(BattleSettings { companions_player_commanded: true, ..settings(rules) }), true);
    let (saver, messages) = SaverHandle::channel();
    world.set_progress_saver(saver);
    (world, messages, calls)
}

/// The only saver request before combat is a coherent read. Installing both its answer
/// and each client's hold acknowledgment starts the engine without saving a prefix.
fn acknowledge_start(world: &mut World, messages: &mut mpsc::UnboundedReceiver<Msg>, seats: &mut [&mut Seat], now: Instant)
    -> BattleId {
    let (accounts, done) = exact_battle_snapshot(messages);
    assert_eq!(accounts, seats.iter().map(|seat| seat.account).collect::<Vec<_>>());
    let states = exact_battle_states(world, &accounts);
    assert!(messages.try_recv().is_err(), "prebattle must queue no story or resource write");
    for seat in seats.iter_mut() {
        let token = exact_battle_pending(seat);
        assert!(world.ordinary_battle_pending_ack(seat.entity, token, now));
    }
    done.send(Ok(states)).expect("initial coherent snapshot receiver");
    world.advance_ordinary_battles(now);
    assert!(messages.try_recv().is_err(), "starting the engine must not save its intermediate story");
    world.players.get(&seats[0].entity).unwrap().battle.expect("the engine started")
}

fn assert_checked_once(calls: &Calls, expected: &[u8]) {
    assert_eq!(calls.actions.load(Ordering::SeqCst), 1, "one action evaluation owns the policy, roster and victory");
    assert_eq!(*calls.checked_parties.lock().unwrap(), vec![expected.to_vec()]);
}

#[test]
fn the_combined_action_uses_its_ordered_roster_but_commits_original_to_victory_only() {
    let now = Instant::now();
    let before = native_story(Some([0, 1, 255]));
    let at_battle = native_story(Some([2, 255, 0]));
    let victory = native_story(Some([1, 255, 0]));
    let rules = SyntheticRules { enemy_hp: 100, ..SyntheticRules::default() };
    let (mut world, mut messages, calls) = snapshot_world(rules, &before, Some(at_battle.clone()), &victory);
    let mut player = seat_story(&mut world, "Player", now, seeded_with(&[(20, 2)]), before.clone());
    player.drain();
    assert!(world.start_story_battle(player.entity, 7, 3, now));
    assert_checked_once(&calls, &at_battle);
    assert_eq!(world.players.get(&player.entity).unwrap().progression, before);
    exact_battle_assert_no_completion(&player.drain());
    let id = acknowledge_start(&mut world, &mut messages, &mut [&mut player], now);
    let roster = roster_of(&mut player);
    assert_eq!(roster.iter().map(|member| (member.is_companion(), member.character)).collect::<Vec<_>>(),
        [(true, 2), (true, 0), (false, 0)], "the battle snapshot's slot order replaces the saved Cloud/Barret pair");
    assert!(player.log.iter().any(|message| matches!(message, ServerMsg::EncounterStart {
        escape_policy: fmv_protocol::EscapePolicy::Forbidden, victory_fanfare: false, ..
    })), "policy comes from the same combined evaluation");
    assert_eq!(world.players.get(&player.entity).unwrap().progression, before);

    // A deterministic item defeats the single synthetic enemy. Its ordinary FIFO debit
    // is allowed; the intermediate story is still neither mirrored nor queued for saving.
    let ready = until_ready(&mut world, id, ActorId(2), now);
    player.drain();
    assert_eq!(world.battle_command(player.entity, id, ActorId(2),
        Command::Item { item: 20, target: Target::One(ActorId(3)) }, ready), BattleReply::Done);
    match messages.try_recv().expect("accepted item debit precedes terminal read") {
        Msg::Save(job) => assert_eq!(job, crate::saver::SaveJob::used_item(player.account, 0, 20)),
        _ => panic!("only the accepted item may be saved before the atomic victory"),
    }
    let (accounts, done) = exact_battle_snapshot(&mut messages);
    let terminal = exact_battle_states(&world, &accounts);
    assert_eq!(terminal[0].pair.story.blob, before);
    assert_eq!(terminal[0].pair.character.as_ref().unwrap().inventory, Some(vec![InventoryEntry::new(20, 1)]));
    done.send(Ok(terminal)).expect("terminal coherent snapshot receiver");
    world.advance_ordinary_battles(ready);
    let (job, done) = exact_battle_outcome(&mut messages);
    assert_eq!(job.owner, player.account);
    assert_eq!(job.expected_story, before, "the transaction compares the original story, not the battle prefix");
    assert_eq!(job.next_story, victory, "only the action's final victory is durable");
    assert_eq!(job.members[0].expected.story.blob, before);
    assert!(job.members[0].battle.consumed_items.is_empty(), "the FIFO debit is not repeated at settlement");
    assert_eq!(world.players.get(&player.entity).unwrap().progression, before);
    exact_battle_assert_no_completion(&player.drain());
    done.send(Ok(exact_battle_committed(&job, &rules))).expect("atomic outcome receiver");
    world.advance_ordinary_battles(ready);
    let published = player.drain();
    assert_eq!(published.iter().filter(|message| matches!(message,
        ServerMsg::StoryUpdate { progress, .. } if progress == &victory)).count(), 1);
    assert!(published.iter().any(|message| matches!(message, ServerMsg::BattleEnd { outcome: Outcome::Victory, .. })));
    assert_eq!(world.players.get(&player.entity).unwrap().progression, victory);
    assert!(!player.log.iter().any(|message| matches!(message,
        ServerMsg::StoryUpdate { progress, .. } if progress == &at_battle)), "the prefix was never published as progress");
    assert_checked_once(&calls, &at_battle);
    assert!(messages.try_recv().is_err(), "no independent prefix or repeated outcome write");
}

#[test]
fn a_known_empty_battle_snapshot_keeps_only_the_player_instead_of_falling_back() {
    let now = Instant::now();
    let before = native_story(Some([0, 1, 255]));
    let empty = native_story(Some([255; 3]));
    let (mut world, mut messages, calls) = snapshot_world(SyntheticRules::default(), &before, Some(empty.clone()), &before);
    let mut player = seat_story(&mut world, "Player", now, ProgressSeed::of(2, None), before.clone());
    player.drain();
    assert!(world.start_story_battle(player.entity, 7, 3, now));
    acknowledge_start(&mut world, &mut messages, &mut [&mut player], now);
    let roster = roster_of(&mut player);
    assert_eq!(roster.len(), 1);
    assert_eq!((roster[0].account(), roster[0].character), (Some(player.account), 2));
    assert!(!roster[0].is_companion());
    assert_eq!(world.players.get(&player.entity).unwrap().progression, before);
    assert_checked_once(&calls, &empty);
}

#[test]
fn an_unknown_or_malformed_battle_snapshot_refuses_before_holds_or_saver_requests() {
    for snapshot in [native_story(None), vec![254]] {
        let now = Instant::now();
        let before = native_story(Some([0, 1, 255]));
        let (mut world, mut messages, calls) = snapshot_world(SyntheticRules::default(), &before, Some(snapshot.clone()), &before);
        let mut player = seat_story(&mut world, "Player", now, ProgressSeed::default(), before.clone());
        player.drain();
        assert!(!world.start_story_battle(player.entity, 7, 3, now));
        assert_checked_once(&calls, &snapshot);
        assert_eq!(world.battle_count(), 0);
        assert!(!world.in_battle(player.entity) && !world.ordinary_battle_account_held(player.account));
        assert!(world.players.get(&player.entity).unwrap().story_pending.is_none());
        assert_eq!(world.players.get(&player.entity).unwrap().progression, before);
        assert!(player.drain().is_empty(), "unknown is not an empty party and cannot publish a hold or encounter");
        assert!(messages.try_recv().is_err(), "a refused roster queues neither baseline nor outcome");
    }
}

#[test]
fn a_requested_formation_that_differs_from_the_action_refuses_before_roster_lookup() {
    let now = Instant::now();
    let before = native_story(Some([0, 1, 255]));
    let snapshot = native_story(Some([2, 255, 0]));
    let (mut world, mut messages, calls) = snapshot_world(SyntheticRules::default(), &before, Some(snapshot), &before);
    let mut player = seat_story(&mut world, "Player", now, ProgressSeed::default(), before.clone());
    player.drain();
    assert!(!world.start_story_battle(player.entity, 7, 1, now), "the action authorizes formation 3 only");
    assert_eq!(calls.actions.load(Ordering::SeqCst), 1);
    assert!(calls.checked_parties.lock().unwrap().is_empty(), "policy disagreement refuses before querying the battle roster");
    assert_eq!(world.battle_count(), 0);
    assert!(!world.in_battle(player.entity) && !world.ordinary_battle_account_held(player.account));
    assert!(world.players.get(&player.entity).unwrap().story_pending.is_none());
    assert_eq!(world.players.get(&player.entity).unwrap().progression, before);
    assert!(player.drain().is_empty(), "no hold or encounter was published");
    assert!(messages.try_recv().is_err(), "no snapshot or write was queued");
}

#[test]
fn no_intermediate_snapshot_keeps_the_online_leaders_roster_for_a_follower_trigger() {
    let now = Instant::now();
    let leader_story = native_story(Some([0, 1, 255]));
    let follower_story = native_story(Some([2, 255, 255]));
    let (mut world, mut messages, calls) = snapshot_world(SyntheticRules::default(), &follower_story, None, &follower_story);
    let mut leader = seat_story(&mut world, "Leader", now, ProgressSeed::of(0, None), leader_story.clone());
    let mut follower = seat_story(&mut world, "Follower", now, ProgressSeed::of(2, None), follower_story.clone());
    party_up(&mut world, &leader, &follower, now);
    leader.drain(); follower.drain();
    assert!(world.start_story_battle(follower.entity, 7, 3, now));
    acknowledge_start(&mut world, &mut messages, &mut [&mut leader, &mut follower], now);
    let roster = roster_of(&mut follower);
    assert_eq!(roster, roster_of(&mut leader));
    assert_eq!(roster.iter().filter(|member| member.is_companion()).map(|member| member.character).collect::<Vec<_>>(), [0, 1]);
    assert_eq!(roster.iter().filter_map(BattleMember::account).collect::<Vec<_>>(), [leader.account, follower.account]);
    assert_eq!(world.players.get(&leader.entity).unwrap().progression, leader_story);
    assert_eq!(world.players.get(&follower.entity).unwrap().progression, follower_story);
    assert_checked_once(&calls, &leader_story);
}
