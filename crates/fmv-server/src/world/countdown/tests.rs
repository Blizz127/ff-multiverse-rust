use std::time::{Duration, Instant};

use fmv_adapter_api::GameAdapter;
use fmv_core::{AccountId, AnimState, AvatarAppearance, Pos};
use fmv_fixture::rooms::{RoomsAdapter, COUNTDOWN_SECS, HALL, STORY_ADVANCED, STORY_CLEARED};
use fmv_party::PartyConfig;
use tokio::sync::{mpsc, watch};

use super::super::{MoveOutcome, NewPlayer, Outbound, PartyOutcome};
use super::*;
use crate::saver::{Msg, SaverHandle};

struct Seat {
    entity: EntityId,
    account: AccountId,
    rx: mpsc::Receiver<Outbound>,
}

impl Seat {
    fn drain(&mut self) -> Vec<ServerMsg> {
        let mut out = Vec::new();
        while let Ok(m) = self.rx.try_recv() {
            out.push((*m).clone());
        }
        out
    }

    /// The countdown messages among what the seat was sent since the last call.
    fn countdowns(&mut self) -> Vec<CountdownState> {
        self.drain()
            .into_iter()
            .filter_map(|m| match m {
                ServerMsg::Countdown { state } => Some(state),
                _ => None,
            })
            .collect()
    }
}

/// A world over the countdown rooms, with a saver queue the test can read.
fn world() -> (World, mpsc::UnboundedReceiver<Msg>) {
    let adapter: Arc<dyn GameAdapter> = Arc::new(RoomsAdapter::with_a_countdown());
    let mut w = World::with_party_config(adapter.load_area(HALL).unwrap(), PartyConfig::default()).unwrap();
    w.set_area_source(adapter);
    let (handle, rx) = SaverHandle::channel();
    w.set_progress_saver(handle);
    (w, rx)
}

fn seat(w: &mut World, name: &str, progression: &[u8], now: Instant) -> Seat {
    let (session, entity) = w.allocate_ids();
    let (tx, rx) = mpsc::channel(256);
    let close = Arc::new(watch::channel(false).0);
    let avatar = AvatarAppearance { display_name: name.into(), tint: [1, 2, 3], appearance_ref: None };
    let account = AccountId(entity.0);
    w.join(NewPlayer { session, entity, account, avatar, progression: progression.to_vec(), progress: Default::default(), scene_state: None, scene_group: Vec::new(), tx, close }, now).expect("joins");
    Seat { entity, account, rx }
}

/// Hop to (x, y) after the budget had 700 ms to refill (the world's clock is `now`).
fn go(w: &mut World, s: &Seat, x: f32, y: f32, now: &mut Instant) {
    *now += Duration::from_millis(700);
    assert_eq!(w.apply_move(s.entity, Pos::new(x, y, 0.0), 0.0, AnimState::Walk, *now), MoveOutcome::Applied);
}

/// The story region (the countdown's start) from the spawn at (-100, 0).
fn step_into_the_story_region(w: &mut World, s: &Seat, now: &mut Instant) {
    go(w, s, 100.0, 0.0, now);
    go(w, s, 200.0, 0.0, now);
}

/// The clear region (the story's next step) from the story region.
fn walk_to_the_clear_region(w: &mut World, s: &Seat, now: &mut Instant) {
    go(w, s, 0.0, 100.0, now);
    go(w, s, -250.0, 150.0, now);
}

fn log_text(w: &mut World) -> String {
    w.take_logs().iter().map(|l| l.text().to_string()).collect::<Vec<_>>().join("\n")
}

fn saved_stories(rx: &mut mpsc::UnboundedReceiver<Msg>) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    while let Ok(msg) = rx.try_recv() {
        if let Msg::Story(job) = msg {
            out.push(job.progress);
        }
    }
    out
}

fn running(remaining: u64, total: u64) -> CountdownState {
    CountdownState::Running { remaining_ms: u32::try_from(remaining).unwrap(), total_ms: u32::try_from(total).unwrap() }
}

fn over(reason: CountdownEnd) -> CountdownState {
    CountdownState::Over { reason }
}

fn party(w: &mut World, leader: &Seat, member: &Seat, now: Instant) {
    assert_eq!(w.party_invite(leader.entity, member.entity, now), PartyOutcome::Done);
    let invite = w.parties.invites().next().expect("an invite").id;
    assert_eq!(w.party_accept(member.entity, invite, now), PartyOutcome::Done);
}

const TOTAL_MS: u64 = COUNTDOWN_SECS as u64 * 1000;

#[test]
fn a_step_that_starts_a_countdown_tells_its_player_the_whole_length_once_and_logs_it() {
    let t0 = Instant::now();
    let (mut w, _saved) = world();
    let mut a = seat(&mut w, "A", &[], t0);
    assert!(a.countdowns().is_empty(), "a new account is told nothing");
    w.take_logs();
    let mut now = t0;
    step_into_the_story_region(&mut w, &a, &mut now);

    // the story's own messages first, then the countdown: the whole length (nothing has ticked yet)
    let msgs = a.drain();
    let kinds: Vec<&str> = msgs
        .iter()
        .map(|m| match m {
            ServerMsg::StoryUpdate { advance: Some(_), .. } => "step",
            ServerMsg::AreaSnapshot { .. } => "snapshot",
            ServerMsg::InstancePlacement { .. } => "placement",
            ServerMsg::Countdown { .. } => "countdown",
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(kinds, ["step", "snapshot", "placement", "countdown"], "{msgs:?}");
    assert!(matches!(msgs.last(), Some(ServerMsg::Countdown { state }) if *state == running(TOTAL_MS, TOTAL_MS)), "{msgs:?}");
    assert_eq!(w.countdown_count(), 1);
    let log = log_text(&mut w);
    assert!(log.contains(&format!("countdown started account={} seconds={COUNTDOWN_SECS}", a.account.0)), "{log}");

    // the player knows: another settling (a walk, a tick) says nothing more
    go(&mut w, &a, 210.0, 10.0, &mut now);
    w.advance_countdowns(now);
    w.advance_tick();
    assert!(a.countdowns().is_empty());
    assert!(!log_text(&mut w).contains("countdown"));
}

#[test]
fn a_member_sees_its_leaders_countdown_and_stops_seeing_it_when_it_leaves_the_party_and_sees_what_is_left_when_it_returns() {
    let t0 = Instant::now();
    let (mut w, _saved) = world();
    let mut a = seat(&mut w, "A", &[], t0);
    let mut b = seat(&mut w, "B", &[], t0);
    party(&mut w, &a, &b, t0);
    a.drain();
    b.drain();
    let mut now = t0;
    step_into_the_story_region(&mut w, &a, &mut now);
    // both are told it at the start (B follows A's story: it is in A's party)
    assert_eq!(a.countdowns(), [running(TOTAL_MS, TOTAL_MS)]);
    assert_eq!(b.countdowns(), [running(TOTAL_MS, TOTAL_MS)]);
    // a tick sets the deadline; 30 s later B leaves: it is told the countdown is not its own any more
    w.advance_countdowns(now);
    let later = now + Duration::from_secs(30);
    w.advance_countdowns(later);
    assert_eq!(w.party_leave(b.entity), PartyOutcome::Done);
    assert_eq!(b.countdowns(), [over(CountdownEnd::Cleared)]);
    assert!(a.countdowns().is_empty(), "A's own countdown goes on");
    assert_eq!(w.countdown_count(), 1);
    // B comes back: it is told what is left on the clock
    party(&mut w, &a, &b, later);
    assert_eq!(b.countdowns(), [running(TOTAL_MS - 30_000, TOTAL_MS)]);
    // and a player that was never told one is told nothing by leaving
    assert!(a.countdowns().is_empty());
}

#[test]
fn a_countdown_that_runs_out_tells_everyone_once_changes_nothing_and_leaves_the_story_to_go_on() {
    let t0 = Instant::now();
    let (mut w, mut saved) = world();
    let mut a = seat(&mut w, "A", &[], t0);
    let mut b = seat(&mut w, "B", &[], t0);
    party(&mut w, &a, &b, t0);
    let mut now = t0;
    step_into_the_story_region(&mut w, &a, &mut now);
    assert_eq!(saved_stories(&mut saved), [STORY_ADVANCED.to_vec()], "the step itself is saved, once");
    a.drain();
    b.drain();
    w.take_logs();

    // the first tick sets the deadline; nothing happens before it
    w.advance_countdowns(now);
    w.advance_countdowns(now + Duration::from_secs(u64::from(COUNTDOWN_SECS) - 1));
    assert!(a.countdowns().is_empty() && b.countdowns().is_empty() && w.countdown_count() == 1);
    // it runs out: both are told, one log line, and nothing else
    w.advance_countdowns(now + Duration::from_secs(u64::from(COUNTDOWN_SECS)));
    assert_eq!(a.countdowns(), [over(CountdownEnd::TimeUp)]);
    assert_eq!(b.countdowns(), [over(CountdownEnd::TimeUp)]);
    assert_eq!(w.countdown_count(), 0);
    let log = log_text(&mut w);
    assert_eq!(log.lines().filter(|l| l.contains("countdown over")).count(), 1, "{log}");
    assert!(log.contains(&format!("countdown over account={} reason=time-up", a.account.0)), "{log}");
    assert!(!log.contains("story advanced"), "nothing moved the story: {log}");
    // no progress was saved, wiped or reset by it, and the players' stories are what they were
    assert!(saved_stories(&mut saved).is_empty());
    assert_eq!(w.players.get(&a.entity).unwrap().progression, STORY_ADVANCED);
    assert!(w.players.get(&b.entity).unwrap().progression.is_empty(), "the member's own story was never touched");
    // ... and it is said once: later ticks and settlings tell nobody anything
    w.advance_countdowns(now + Duration::from_secs(10_000));
    go(&mut w, &a, 210.0, 10.0, &mut now);
    assert!(a.countdowns().is_empty() && b.countdowns().is_empty());
    // the story goes on for a party that was late: the next step applies as ever (and starts no countdown)
    walk_to_the_clear_region(&mut w, &a, &mut now);
    assert_eq!(w.players.get(&a.entity).unwrap().progression, STORY_CLEARED);
    assert_eq!(saved_stories(&mut saved), [STORY_CLEARED.to_vec()]);
    assert!(a.countdowns().is_empty() && w.countdown_count() == 0);
}

#[test]
fn the_story_moving_past_it_ends_the_countdown_for_the_party_and_it_is_not_said_to_run_out_afterwards() {
    let t0 = Instant::now();
    let (mut w, _saved) = world();
    let mut a = seat(&mut w, "A", &[], t0);
    let mut b = seat(&mut w, "B", &[], t0);
    party(&mut w, &a, &b, t0);
    let mut now = t0;
    step_into_the_story_region(&mut w, &a, &mut now);
    w.advance_countdowns(now);
    a.drain();
    b.drain();
    w.take_logs();
    // A gets to the clear region: its story moves on (1 -> 2), which makes the countdown moot
    walk_to_the_clear_region(&mut w, &a, &mut now);
    assert_eq!(a.countdowns(), [over(CountdownEnd::Cleared)]);
    assert_eq!(b.countdowns(), [over(CountdownEnd::Cleared)]);
    assert_eq!(w.countdown_count(), 0);
    let log = log_text(&mut w);
    assert!(log.contains(&format!("countdown over account={} reason=cleared", a.account.0)), "{log}");
    // the clock that would have run out does not say so
    w.advance_countdowns(now + Duration::from_secs(10_000));
    assert!(a.countdowns().is_empty() && b.countdowns().is_empty());
    assert!(!log_text(&mut w).contains("time-up"));
}

#[test]
fn a_leader_who_leaves_the_world_ends_the_countdown_for_the_members_it_governed() {
    let t0 = Instant::now();
    let (mut w, _saved) = world();
    let mut a = seat(&mut w, "A", &[], t0);
    let mut b = seat(&mut w, "B", &[], t0);
    party(&mut w, &a, &b, t0);
    let mut now = t0;
    step_into_the_story_region(&mut w, &a, &mut now);
    b.drain();
    a.drain();
    // (B was told: it is in A's party) A logs out
    assert!(w.leave(a.entity));
    assert_eq!(w.countdown_count(), 0, "its clock went with it");
    let told = b.countdowns();
    assert!(told.contains(&over(CountdownEnd::Cleared)), "B was told it is over: {told:?}");
    w.advance_countdowns(now + Duration::from_secs(10_000));
    assert!(b.countdowns().is_empty());
}

#[test]
fn a_new_step_with_a_countdown_replaces_the_one_that_runs_and_tells_the_players_again() {
    let t0 = Instant::now();
    let (mut w, _saved) = world();
    let mut a = seat(&mut w, "A", &[], t0);
    let mut now = t0;
    step_into_the_story_region(&mut w, &a, &mut now);
    w.advance_countdowns(now);
    w.advance_countdowns(now + Duration::from_secs(40));
    a.drain();
    // (another step that starts one, by the adapter's own say: the world replaces it and tells the account)
    let step = StoryStep { progress: STORY_ADVANCED.to_vec(), summary: "again".into(), countdown: Some(30) };
    let area = w.players.get(&a.entity).unwrap().area.clone();
    w.countdown_after_step(a.account, &area, &step);
    let mut failed = Vec::new();
    w.sync_countdown(a.entity, &mut failed);
    assert!(failed.is_empty());
    assert_eq!(a.countdowns(), [running(30_000, 30_000)]);
    assert_eq!(w.countdown_count(), 1);
}

#[test]
fn an_account_that_logs_in_at_the_story_a_countdown_started_in_has_none_because_a_countdown_is_not_stored() {
    let t0 = Instant::now();
    let (mut w, _saved) = world();
    let mut a = seat(&mut w, "A", STORY_ADVANCED, t0);
    // (it is told where its story stands, as ever, and nothing of a countdown)
    let msgs = a.drain();
    assert!(msgs.iter().any(|m| matches!(m, ServerMsg::StoryUpdate { advance: None, .. })), "{msgs:?}");
    assert!(msgs.iter().all(|m| !matches!(m, ServerMsg::Countdown { .. })), "{msgs:?}");
    assert_eq!(w.countdown_count(), 0);
    w.advance_countdowns(t0 + Duration::from_secs(10_000));
    assert!(a.countdowns().is_empty());
}
