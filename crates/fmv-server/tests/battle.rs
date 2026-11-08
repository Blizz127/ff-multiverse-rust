//! End-to-end tests of shared battles (protocol v3) over real TCP sockets against an in-process
//! server. The area is the FIXTURE plaza and the battle rules are `fmv_protocol::SyntheticRules`
//! (hand-made fighters): this tests the SERVER's battle machinery (authority, freezing, the input
//! log, rewards), not FF7 data; the real-data checks are in `battle_real.rs` (ignored by default).
//!
//! Every client here replays the battle exactly as the real client does (`BattleReplay`), so a
//! passing test is also a proof that the server's message log is one a replay accepts.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use fmv_core::{AnimState, Pos};
use fmv_ff7_battle::rules::{BattleEvent, Command, Outcome};
use fmv_protocol::{
    ActorId, BattleId, BattleRejection, BattleReplay, BattleReward, BattleRules, BattleSetup, ClientMsg, PartyEvent, ServerMsg,
    SyntheticRules,
};
use fmv_server::{BattleSettings, ServerConfig};
use tokio::time::sleep;

/// Fast battles: speed 0 and 1000 engine ticks per second, so a fight lasts well under a second.
fn fast(debug_encounters: bool) -> ServerConfig {
    ServerConfig {
        battle: Some(BattleSettings {
            battle_speed: 0,
            ticks_per_second: 1000,
            ..BattleSettings::new(Arc::new(SyntheticRules::default()))
        }),
        debug_encounters,
        ..ServerConfig::default()
    }
}

/// A battle that cannot progress during a test (one engine tick per second at the slowest speed),
/// so the first moments of a battle can be examined without a gauge ever filling.
fn frozen_time() -> ServerConfig {
    ServerConfig {
        battle: Some(BattleSettings {
            battle_speed: 255,
            ticks_per_second: 1,
            ..BattleSettings::new(Arc::new(SyntheticRules::default()))
        }),
        debug_encounters: true,
        ..ServerConfig::default()
    }
}

/// `a` invites `b`, `b` accepts, and both have heard the party form.
async fn party_up(a: &mut Joined, b: &mut Joined) {
    a.client.send(&ClientMsg::PartyInvite { target: b.entity }).await;
    let invite = b
        .client
        .until("InviteSent", |m| match m {
            ServerMsg::PartyEvent { event: PartyEvent::InviteSent { invite, .. }, .. } => Some(*invite),
            _ => None,
        })
        .await;
    b.client.send(&ClientMsg::PartyAccept { invite }).await;
    for client in [&mut a.client, &mut b.client] {
        client
            .until("PartyFormed", |m| match m {
                ServerMsg::PartyEvent { event: PartyEvent::PartyFormed { .. }, .. } => Some(()),
                _ => None,
            })
            .await;
    }
}

/// What one player saw of one battle.
#[derive(Debug)]
struct Played {
    start: ServerMsg,
    outcome: Outcome,
    rewards: Vec<BattleReward>,
    /// `BattleEvent` messages received.
    events: usize,
    commands_sent: usize,
    rejections: Vec<BattleRejection>,
    /// `EntityUpdate`s that arrived before the battle ended (nobody should have moved).
    updates_during_battle: usize,
}

/// Play one battle as the real client does: replay every message, attack the first living enemy
/// whenever our gauge is full (one command at a time), and check the end.
async fn play(j: &mut Joined, rules: &SyntheticRules) -> Played {
    let start = j.client.until("EncounterStart", |m| matches!(m, ServerMsg::EncounterStart { .. }).then(|| m.clone())).await;
    let ServerMsg::EncounterStart { battle_id, formation_id, seed, party, preemptive, back_attack, battle_speed, ticks_per_second, data_fingerprint } =
        start.clone()
    else {
        unreachable!()
    };
    assert_eq!(data_fingerprint, rules.data_fingerprint());
    let setup = BattleSetup { formation_id, seed, party: party.clone(), preemptive, back_attack, battle_speed, ticks_per_second };
    let mut replay = BattleReplay::start(rules, &setup).expect("the client builds the battle from its own data");
    let mine = ActorId(party.iter().position(|m| m.entity == j.entity).expect("we are in the party") as u8);
    let (mut pending, mut events, mut commands_sent, mut updates_during_battle) = (false, 0, 0, 0);
    let mut rejections = Vec::new();
    let mut saw_the_outcome = false;
    loop {
        let state = replay.state();
        if !pending && state.outcome().is_none() && state.ready_actors().contains(&mine) {
            let target = state.enemy_ids().into_iter().find(|e| state.actor(*e).is_some_and(|c| c.is_alive())).expect("a living enemy");
            j.client.send(&ClientMsg::BattleCommand { battle_id, actor: mine, command: Command::Attack { target } }).await;
            pending = true;
            commands_sent += 1;
        }
        match j.client.next().await {
            ServerMsg::BattleEvent { battle_id: id, tick, events: evs, command } => {
                assert_eq!(id, battle_id);
                replay.apply(tick, &evs, command.as_ref()).unwrap_or_else(|d| panic!("the replay diverged: {d}"));
                events += 1;
                if command.is_some_and(|c| c.actor == mine) {
                    pending = false;
                }
                saw_the_outcome |= evs.iter().any(|e| matches!(e, BattleEvent::Victory | BattleEvent::Defeat));
            }
            ServerMsg::BattleRejected { reason, .. } => {
                rejections.push(reason);
                pending = false;
            }
            ServerMsg::BattleEnd { battle_id: id, outcome, rewards } => {
                assert_eq!(id, battle_id);
                assert!(saw_the_outcome, "the message that carries the outcome comes before BattleEnd");
                replay.check_end(outcome, &rewards).unwrap_or_else(|d| panic!("the end diverged: {d}"));
                return Played { start, outcome, rewards, events, commands_sent, rejections, updates_during_battle };
            }
            ServerMsg::EntityUpdate { .. } => updates_during_battle += 1,
            _ => {}
        }
    }
}

/// The first `EntityUpdate` that mentions `entity`, with its position.
async fn next_position_of(j: &mut Joined, what: &str) -> Pos {
    let entity = j.entity;
    j.client
        .until(what, |m| match m {
            ServerMsg::EntityUpdate { entities, .. } => entities.iter().find(|e| e.id == entity).map(|e| e.pos),
            _ => None,
        })
        .await
}

// ---------------------------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fixture_server_refuses_battles_clearly_and_keeps_the_player_in_the_field() {
    // The default server has no battle data: battles are unsupported, with or without the debug flag.
    for debug in [false, true] {
        let server = TestServer::start(ServerConfig { debug_encounters: debug, ..ServerConfig::default() }).await;
        let mut a = join(server.addr, &uniq("Fx")).await;
        a.client.send(&ClientMsg::RequestEncounter { formation_id: None }).await;
        let (battle, reason) = a
            .client
            .until("BattleRejected", |m| match m {
                ServerMsg::BattleRejected { battle_id, reason } => Some((*battle_id, *reason)),
                _ => None,
            })
            .await;
        assert_eq!((battle, reason), (None, BattleRejection::Unsupported), "debug flag {debug}");
        assert!(reason.to_string().contains("no owned battle data"), "{reason}");
        // A command for a battle that does not exist is refused the same way a stray one always is.
        a.client
            .send(&ClientMsg::BattleCommand { battle_id: BattleId(1), actor: ActorId(0), command: Command::Attack { target: ActorId(1) } })
            .await;
        let reason = a
            .client
            .until("BattleRejected", |m| match m {
                ServerMsg::BattleRejected { reason, .. } => Some(*reason),
                _ => None,
            })
            .await;
        assert_eq!(reason, BattleRejection::NotInBattle);
        // The session is untouched: still connected, still answers a ping, and can move.
        a.client.send(&ClientMsg::Ping { nonce: 7 }).await;
        a.client.until("Pong", |m| matches!(m, ServerMsg::Pong { nonce: 7, .. }).then_some(())).await;
        sleep(Duration::from_millis(200)).await;
        a.client.send(&ClientMsg::MoveIntent { seq: 1, pos: Pos::new(-80.0, 0.0, 0.0), facing: 0.0, anim: AnimState::Walk }).await;
        let moved = next_position_of(&mut a, "a normal move").await;
        assert_eq!(moved.x, -80.0);
        server.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_server_without_debug_encounters_refuses_the_request_and_says_how_to_enable_it() {
    let server = TestServer::start(fast(false)).await;
    let mut a = join(server.addr, &uniq("Nd")).await;
    a.client.send(&ClientMsg::RequestEncounter { formation_id: Some(0) }).await;
    let reason = a
        .client
        .until("BattleRejected", |m| match m {
            ServerMsg::BattleRejected { reason, .. } => Some(*reason),
            _ => None,
        })
        .await;
    assert_eq!(reason, BattleRejection::DebugDisabled);
    assert!(reason.to_string().contains("--debug-encounters"));
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn commands_are_refused_for_other_peoples_characters_unready_gauges_and_unknown_battles() {
    let server = TestServer::start(frozen_time()).await;
    let mut a = join(server.addr, &uniq("Ca")).await;
    let mut b = join(server.addr, &uniq("Cb")).await;
    let mut outsider = join(server.addr, &uniq("Co")).await;
    party_up(&mut a, &mut b).await;
    a.client.send(&ClientMsg::RequestEncounter { formation_id: Some(0) }).await;
    let mut start = Vec::new();
    for client in [&mut a.client, &mut b.client] {
        start.push(client.until("EncounterStart", |m| matches!(m, ServerMsg::EncounterStart { .. }).then(|| m.clone())).await);
    }
    assert_eq!(start[0], start[1], "both members receive the very same start");
    let ServerMsg::EncounterStart { battle_id, party, formation_id, .. } = &start[0] else { unreachable!() };
    assert_eq!((*formation_id, party.len()), (0, 2));
    assert_eq!((party[0].entity, party[1].entity), (a.entity, b.entity));
    assert_eq!((party[0].account, party[1].account), (a.account, b.account));
    let attack = |actor: u8| ClientMsg::BattleCommand { battle_id: *battle_id, actor: ActorId(actor), command: Command::Attack { target: ActorId(2) } };

    async fn refusal(j: &mut Joined, msg: ClientMsg) -> (Option<BattleId>, BattleRejection) {
        j.client.send(&msg).await;
        j.client
            .until("BattleRejected", |m| match m {
                ServerMsg::BattleRejected { battle_id, reason } => Some((*battle_id, *reason)),
                _ => None,
            })
            .await
    }

    // B may not command A's character, and A may not command B's.
    assert_eq!(refusal(&mut b, attack(0)).await, (Some(*battle_id), BattleRejection::NotYourActor));
    assert_eq!(refusal(&mut a, attack(1)).await, (Some(*battle_id), BattleRejection::NotYourActor));
    // Your own character, but its gauge is not full (nothing can fill in this server's time).
    assert_eq!(refusal(&mut a, attack(0)).await, (Some(*battle_id), BattleRejection::GaugeNotFull));
    assert_eq!(refusal(&mut b, attack(1)).await, (Some(*battle_id), BattleRejection::GaugeNotFull));
    // A battle that is not yours: somebody outside it, and an id nobody has.
    assert_eq!(refusal(&mut outsider, attack(0)).await, (Some(*battle_id), BattleRejection::NotInBattle));
    let ghost = ClientMsg::BattleCommand { battle_id: BattleId(4_000), actor: ActorId(0), command: Command::Attack { target: ActorId(2) } };
    assert_eq!(refusal(&mut a, ghost).await, (Some(BattleId(4_000)), BattleRejection::NotInBattle));
    // Asking for a second battle while in one, and for a formation that does not exist.
    assert_eq!(refusal(&mut a, ClientMsg::RequestEncounter { formation_id: Some(0) }).await, (None, BattleRejection::AlreadyInBattle));
    assert_eq!(refusal(&mut outsider, ClientMsg::RequestEncounter { formation_id: Some(999) }).await, (None, BattleRejection::UnknownFormation));
    // Every refusal was routine: all sessions are alive.
    for j in [&mut a, &mut b, &mut outsider] {
        j.client.send(&ClientMsg::Ping { nonce: 1 }).await;
        j.client.until("Pong", |m| matches!(m, ServerMsg::Pong { .. }).then_some(())).await;
    }
    server.shutdown().await;
}

// ---------------------------------------------------------------------------------------------
// A shared battle, start to finish
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_players_in_a_party_fight_one_battle_win_it_and_both_return_to_the_field() {
    let rules = SyntheticRules::default();
    let server = TestServer::start(fast(true)).await;
    let mut a = join(server.addr, &uniq("Ba")).await;
    let mut b = join(server.addr, &uniq("Bb")).await;
    let mut bystander = join(server.addr, &uniq("Bc")).await;
    party_up(&mut a, &mut b).await;
    let spawn_a = Pos::new(-100.0, 0.0, 0.0);

    // A frozen player's movement is not applied: A tries to walk while the battle starts and runs.
    a.client.send(&ClientMsg::RequestEncounter { formation_id: Some(1) }).await;
    a.client.send(&ClientMsg::MoveIntent { seq: 1, pos: Pos::new(-50.0, 0.0, 0.0), facing: 0.0, anim: AnimState::Walk }).await;

    let (pa, pb) = tokio::join!(play(&mut a, &rules), play(&mut b, &rules));

    // The same battle for both: one start, one outcome, one list of rewards.
    assert_eq!(pa.start, pb.start);
    let ServerMsg::EncounterStart { battle_id, formation_id, party, preemptive, back_attack, .. } = &pa.start else { unreachable!() };
    assert_eq!((*battle_id, *formation_id, party.len(), *preemptive, *back_attack), (BattleId(1), 1, 2, false, false));
    assert_eq!((pa.outcome, pb.outcome), (Outcome::Victory, Outcome::Victory));
    assert_eq!(pa.rewards, pb.rewards);
    assert_eq!(pa.rewards.len(), 2, "one entry per party member");
    assert_eq!(pa.rewards.iter().map(|r| r.account).collect::<Vec<_>>(), vec![a.account, b.account]);
    // Two dummies (12 exp, 20 gil, 2 ap each): 24, 40 and 4 shared by the living.
    assert_eq!(pa.rewards.iter().map(|r| r.exp).sum::<u32>(), 24);
    assert_eq!(pa.rewards.iter().map(|r| r.gil).sum::<u32>(), 40);
    // Both attacked, and the server never had to refuse a command (each waited for its answer).
    assert!(pa.commands_sent >= 1 && pb.commands_sent >= 1, "{} and {} attacks", pa.commands_sent, pb.commands_sent);
    assert!(pa.rejections.is_empty() && pb.rejections.is_empty(), "{:?} {:?}", pa.rejections, pb.rejections);
    assert!(pa.events > 2 && pa.events == pb.events, "{} and {} event messages", pa.events, pb.events);
    // Nobody moved while the battle ran, so nothing was broadcast.
    assert_eq!((pa.updates_during_battle, pb.updates_during_battle), (0, 0));

    // The bystander never heard of any of it.
    let heard = bystander.client.drain_for(Duration::from_millis(300)).await;
    assert!(
        heard.iter().all(|m| !matches!(
            m,
            ServerMsg::EncounterStart { .. } | ServerMsg::BattleEvent { .. } | ServerMsg::BattleEnd { .. } | ServerMsg::BattleRejected { .. }
        )),
        "{heard:?}"
    );
    // Back in the field the server told everyone where the members stand (their entities were
    // marked changed when the battle ended): A is still at its spawn point, because the walk it
    // attempted during the battle was ignored, and nobody was ever seen at -50.
    let seen_a: Vec<Pos> = heard
        .iter()
        .filter_map(|m| match m {
            ServerMsg::EntityUpdate { entities, .. } => entities.iter().find(|e| e.id == a.entity).map(|e| e.pos),
            _ => None,
        })
        .collect();
    assert!(!seen_a.is_empty(), "the bystander is told where A stands once the battle is over");
    assert!(seen_a.iter().all(|p| p.dist_xy(spawn_a) < 0.01), "A never moved while it was in battle: {seen_a:?}");

    // A can walk again.
    sleep(Duration::from_millis(300)).await;
    a.client.send(&ClientMsg::MoveIntent { seq: 2, pos: Pos::new(-90.0, 0.0, 0.0), facing: 0.0, anim: AnimState::Walk }).await;
    let walked = a
        .client
        .until("A's own move after the battle", |m| match m {
            ServerMsg::EntityUpdate { entities, .. } => entities.iter().find(|e| e.id == a.entity && e.pos.x != spawn_a.x).map(|e| e.pos),
            ServerMsg::MoveRejected { seq: 2, corrected } => panic!("the first move after the battle was refused (corrected to {corrected:?})"),
            _ => None,
        })
        .await;
    assert_eq!(walked.x, -90.0, "the move of seq 2 was applied, the frozen move of seq 1 (to -50) never was");

    // A fresh battle can start for the same party.
    b.client.send(&ClientMsg::RequestEncounter { formation_id: Some(0) }).await;
    let (second_a, second_b) = tokio::join!(play(&mut a, &rules), play(&mut b, &rules));
    let ServerMsg::EncounterStart { battle_id: second, .. } = second_a.start else { unreachable!() };
    assert_eq!(second, BattleId(2), "battle ids are never reused");
    assert_eq!((second_a.outcome, second_b.outcome), (Outcome::Victory, Outcome::Victory));
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_solo_player_can_fight_and_win_alone_and_a_hopeless_fight_is_a_defeat_without_rewards() {
    let rules = SyntheticRules::default();
    let server = TestServer::start(fast(true)).await;
    let mut a = join(server.addr, &uniq("Sa")).await;
    a.client.send(&ClientMsg::RequestEncounter { formation_id: None }).await;
    let won = play(&mut a, &rules).await;
    let ServerMsg::EncounterStart { formation_id, party, .. } = &won.start else { unreachable!() };
    assert_eq!((*formation_id, party.len()), (0, 1), "no formation named: the rules' default, for a party of one");
    assert_eq!(won.outcome, Outcome::Victory);
    assert_eq!(won.rewards, vec![BattleReward { account: a.account, exp: 12, gil: 20, ap: 2 }]);

    // Formation 2 is overwhelming.
    a.client.send(&ClientMsg::RequestEncounter { formation_id: Some(2) }).await;
    let lost = play(&mut a, &rules).await;
    assert_eq!(lost.outcome, Outcome::Defeat);
    assert!(lost.rewards.is_empty());
    // A lost battle frees the player all the same.
    a.client.send(&ClientMsg::RequestEncounter { formation_id: Some(0) }).await;
    assert_eq!(play(&mut a, &rules).await.outcome, Outcome::Victory);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_who_disconnects_mid_battle_stays_on_the_roster_and_the_others_finish() {
    let rules = SyntheticRules::default();
    let server = TestServer::start(fast(true)).await;
    let mut a = join(server.addr, &uniq("Da")).await;
    let mut b = join(server.addr, &uniq("Db")).await;
    party_up(&mut a, &mut b).await;
    a.client.send(&ClientMsg::RequestEncounter { formation_id: Some(1) }).await;
    b.client.until("EncounterStart", |m| matches!(m, ServerMsg::EncounterStart { .. }).then_some(())).await;
    let b_account = b.account;
    drop(b); // the socket closes: the server drops the player mid-battle

    let played = play(&mut a, &rules).await;
    assert_eq!(played.outcome, Outcome::Victory, "A wins alone");
    // The roster, and so the reward list, still names both members (its split does not depend on who
    // is connected: `play` already checked that A's replay computes exactly this list).
    assert_eq!(played.rewards.len(), 2);
    assert!(played.rewards.iter().any(|r| r.account == b_account));
    server.shutdown().await;
}
