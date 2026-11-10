//! The race of the first reactor's lift on the owner's REAL data (FF7 Steam Edition), through the world: the real
//! `elevtr1` with its four exits on one line, the real step of `nrthmk`'s finale, the real placements. Never runs by
//! default: it needs the owner's `flevel.lgp` (read-only) and is `#[ignore]`d.
//!
//! ```text
//! FMV_FF7_FLEVEL=/path/to/flevel.lgp cargo test -p fmv-server --lib follow -- --ignored --nocapture
//! ```
//!
//! Nothing from the archive is printed or asserted but field names, positions, moments and bits.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fmv_adapter_api::GameAdapter;
use fmv_core::{AccountId, AnimState, AvatarAppearance, EntityId, Pos};
use fmv_ff7::story::StoryState;
use fmv_party::PartyConfig;
use fmv_protocol::ServerMsg;
use tokio::sync::{mpsc, watch};

use super::super::{MoveOutcome, NewPlayer, Outbound, PartyOutcome, TravelReply};
use super::*;
use crate::ff7::{open_ff7, Ff7Labels, Ff7Source, SharedFf7};

struct Seat {
    entity: EntityId,
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
}

/// The story with the lift's bit set (what the ride down leaves) or not.
fn state(moment: u16, lift_down: bool) -> Vec<u8> {
    let mut s = StoryState::at(moment);
    if lift_down {
        assert!(s.set_bit(1, 225, 0, true));
    }
    s.encode()
}

fn seat(w: &mut World, name: &str, progression: &[u8], now: Instant) -> Seat {
    let (session, entity) = w.allocate_ids();
    let (tx, rx) = mpsc::channel(256);
    let close = Arc::new(watch::channel(false).0);
    let avatar = AvatarAppearance { display_name: name.into(), tint: [1, 2, 3], appearance_ref: None };
    let account = AccountId(entity.0);
    w.join(NewPlayer { session, entity, account, avatar, progression: progression.to_vec(), progress: Default::default(), scene_state: None, scene_group: Vec::new(), tx, close }, now).expect("joins");
    Seat { entity, rx }
}

fn hop(w: &mut World, s: &Seat, x: f32, y: f32, now: &mut Instant) -> MoveOutcome {
    *now += Duration::from_millis(700);
    w.apply_move(s.entity, Pos::new(x, y, 0.0), 0.0, AnimState::Walk, *now)
}

/// From wherever `s` stands in the lift to the other side of the line at x = -185 (the cabin's way out), in hops the
/// speed budget affords.
fn cross_the_lift_line(w: &mut World, s: &Seat, now: &mut Instant) {
    for (x, y) in [(-26.0, -10.0), (-110.0, -10.0), (-230.0, -10.0)] {
        assert_eq!(hop(w, s, x, y, now), MoveOutcome::Applied, "a hop to ({x}, {y}) in the lift");
        if w.transit_target(s.entity).is_some() {
            return;
        }
    }
}

/// The client's answer to the `AreaChange` in `msgs`: it holds the area. Returns where it was offered.
fn answer(w: &mut World, s: &mut Seat, now: Instant) -> (String, (f32, f32)) {
    let msgs = s.drain();
    let offer = msgs.iter().find(|m| matches!(m, ServerMsg::AreaChange { .. })).cloned().expect("an AreaChange");
    let ServerMsg::AreaChange { area, fingerprint, provenance, arrival, .. } = &offer else { unreachable!() };
    assert_eq!(w.join_area(s.entity, area, fingerprint, provenance, now), TravelReply::Done);
    s.drain();
    (area.key.clone(), (arrival.x, arrival.y))
}

#[test]
#[ignore = "needs the owner's real flevel.lgp: set FMV_FF7_FLEVEL"]
fn on_the_real_lift_a_member_that_comes_after_the_leaders_finale_still_rides_to_nmkin_1() {
    let Some(path) = std::env::var_os("FMV_FF7_FLEVEL").map(PathBuf::from) else {
        eprintln!("skipped: FMV_FF7_FLEVEL is not set");
        return;
    };
    let opened = open_ff7(&Ff7Source::Flevel { path, labels: Ff7Labels::default() }).expect("the archive opens");
    let adapter: Arc<dyn GameAdapter> = Arc::new(SharedFf7(opened.adapter.clone()));
    // the world's entry area is the lift itself: everybody who joins stands in it (no walk down the mission needed)
    let mut w = World::with_party_config(adapter.load_area("elevtr1").unwrap(), PartyConfig::default()).unwrap();
    w.set_area_source(adapter.clone());
    let t0 = Instant::now();
    // the party climbs out: moment 27, the lift's bit clear (the ride back up has been pressed)
    let mut leader = seat(&mut w, "Leader", &state(27, false), t0);
    let mut member = seat(&mut w, "Member", &[], t0);
    let mut solo = seat(&mut w, "Solo", &state(33, true), t0);
    assert_eq!(w.party_invite(leader.entity, member.entity, t0), PartyOutcome::Done);
    let invite = w.parties.invites().next().expect("an invite").id;
    assert_eq!(w.party_accept(member.entity, invite, t0), PartyOutcome::Done);
    for s in [&mut leader, &mut member, &mut solo] {
        s.drain();
    }
    w.take_logs();
    let mut now = t0;

    // the leader rides the lift up: the exit that is open for its story leads to nmkin_1
    cross_the_lift_line(&mut w, &leader, &mut now);
    assert_eq!(w.transit_target(leader.entity).as_deref(), Some("nmkin_1"), "the leader's way out");
    let (area, arrival) = answer(&mut w, &mut leader, now);
    assert_eq!((area.as_str(), arrival), ("nmkin_1", (-1485.0, 4298.0)));
    assert_eq!(w.ways_of(leader.entity), 1, "the way is remembered for the member");

    // the leader walks on to `nrthmk` and the finale moves its story from 27 to 33 (the real step of that field's entry
    // point, applied as the world applies it): the member, still in the lift, is moved to the 33 phase of it
    let progression = w.players.get(&leader.entity).map(|p| p.progression.clone()).unwrap();
    let step = adapter.load_area("nrthmk").unwrap().story_advance(&progression, 1).expect("the finale's step applies at 27");
    assert!(step.summary.starts_with("moment=27->33"), "{}", step.summary);
    w.apply_story_step(leader.entity, 1, step);
    let story = StoryState::decode(&w.players.get(&leader.entity).unwrap().progression).expect("a story this adapter reads");
    assert_eq!((story.moment, story.known_bit(1, 225, 0)), (33, Some(true)), "the finale: moment 33, the lift's bit on");
    assert_eq!(w.area_of(member.entity), Some(("elevtr1".to_string(), false)), "the member has not moved");
    member.drain();
    w.take_logs();

    // the member rides the lift only now. Judged on its party's story the lift leads nowhere a server offers (from
    // moment 33 it is the second reactor's: closed, nobody has reviewed it). It goes where its leader went.
    cross_the_lift_line(&mut w, &member, &mut now);
    assert_eq!(w.transit_target(member.entity).as_deref(), Some("nmkin_1"), "the member follows the leader's way out");
    let (area, arrival) = answer(&mut w, &mut member, now);
    assert_eq!((area.as_str(), arrival), ("nmkin_1", (-1485.0, 4298.0)));
    let said: Vec<String> = w.take_logs().iter().map(|l| l.text().to_string()).collect();
    assert!(said.iter().any(|l| l.contains("player follows its leader's way out") && l.contains("area=elevtr1 follows=nmkin_1") && l.contains("story_says=nowhere")), "{said:?}");
    assert!(!said.iter().any(|l| l.contains("smkin")), "nobody was sent to the second reactor: {said:?}");
    assert_eq!(w.players.get(&member.entity).map(|p| p.progression.clone()), Some(Vec::new()), "following moves no story");

    // a player on its own at the same story has no leader to follow: the lift's exits are closed for it, it stays in the lift
    cross_the_lift_line(&mut w, &solo, &mut now);
    assert_eq!(w.transit_target(solo.entity), None, "nothing is open at 33: the way on leads to fields nobody has reviewed");
    assert_eq!(w.area_of(solo.entity), Some(("elevtr1".to_string(), false)));
}
