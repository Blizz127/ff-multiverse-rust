//! Character progress over real TCP sockets (protocol v5, milestone M2-d): each account's own
//! level, experience, AP and gil, paid by won battles, told to the player (`CharacterProgress`, also
//! on joining) and kept in the store across a server restart.
//!
//! The area is the FIXTURE plaza and the rules are `fmv_protocol::SyntheticRules` (hand-made
//! fighters; every character starts at level 5 with 40 experience, level 6 needs 60, level 7 needs
//! 84, and a dummy is worth 12 experience, 20 gil and 2 AP). This tests the SERVER's progress
//! machinery, not FF7 data; the real-data checks are `scripts/battle-smoke.sh --progress`.
//!
//! Every client here replays the battle exactly as the real client does (`BattleReplay`), so a
//! passing test is also a proof that the messages are ones a replay accepts with the levels the
//! progress put on the wire.

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use common::*;
use fmv_ff7_battle::rules::{BattleEvent, Command, Outcome};
use fmv_protocol::{
    ActionRef, ActorId, BattleMember, BattleReplay, BattleReward, BattleSetup, ClientMsg, PartyEvent, RejectCode, ServerMsg,
    SyntheticRules, PROTOCOL_VERSION,
};
use fmv_server::{BattleSettings, ServerConfig};
use tempfile::TempDir;

/// Fast battles (speed 0, 1000 engine ticks a second) on a server that starts them on request,
/// keeping its accounts in `state_dir` (or in a private temporary one).
fn fast(state_dir: Option<PathBuf>) -> ServerConfig {
    ServerConfig {
        battle: Some(BattleSettings {
            battle_speed: 0,
            ticks_per_second: 1000,
            ..BattleSettings::new(Arc::new(SyntheticRules::default()))
        }),
        debug_encounters: true,
        state_dir,
        ..ServerConfig::default()
    }
}

/// One `CharacterProgress` message, as plain numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Progress {
    character: u8,
    level: u8,
    exp: u32,
    exp_to_next: u32,
    gil: u32,
    ap: u32,
    leveled_up: bool,
}

fn progress_of(m: &ServerMsg) -> Option<Progress> {
    match m {
        ServerMsg::CharacterProgress { character, level, exp, exp_to_next, gil, ap, leveled_up } => {
            Some(Progress { character: *character, level: *level, exp: *exp, exp_to_next: *exp_to_next, gil: *gil, ap: *ap, leveled_up: *leveled_up })
        }
        _ => None,
    }
}

async fn next_progress(j: &mut Joined) -> Progress {
    j.client.until("CharacterProgress", progress_of).await
}

/// What a player saw of one battle.
#[derive(Debug)]
struct Fought {
    party: Vec<BattleMember>,
    outcome: Outcome,
    rewards: Vec<BattleReward>,
}

/// Play one battle as the real client does: replay every message, attack the first living enemy
/// whenever our gauge is full (one command at a time), and check the end. Returns at `BattleEnd`.
async fn fight(j: &mut Joined, rules: &SyntheticRules) -> Fought {
    let start = j.client.until("EncounterStart", |m| matches!(m, ServerMsg::EncounterStart { .. }).then(|| m.clone())).await;
    let ServerMsg::EncounterStart { battle_id, formation_id, seed, party, preemptive, back_attack, battle_speed, ticks_per_second, .. } = start
    else {
        unreachable!()
    };
    let setup = BattleSetup { formation_id, seed, party: party.clone(), preemptive, back_attack, battle_speed, ticks_per_second };
    let mut replay = BattleReplay::start(rules, &setup).expect("the client builds the battle from its own data");
    let mine = ActorId(party.iter().position(|m| m.entity == j.entity).expect("we are in the party") as u8);
    let mut pending = false;
    loop {
        let state = replay.state();
        if !pending && state.outcome().is_none() && state.ready_actors().contains(&mine) {
            let target = state.enemy_ids().into_iter().find(|e| state.actor(*e).is_some_and(|c| c.is_alive())).expect("a living enemy");
            j.client.send(&ClientMsg::BattleCommand { battle_id, actor: mine, command: Command::Attack { target } }).await;
            pending = true;
        }
        match j.client.next().await {
            ServerMsg::BattleEvent { tick, events, command, .. } => {
                replay.apply(tick, &events, command.as_ref()).unwrap_or_else(|d| panic!("the replay diverged: {d}"));
                if command.is_some_and(|c| c.actor == mine) {
                    pending = false;
                }
            }
            ServerMsg::BattleRejected { .. } => pending = false,
            ServerMsg::BattleEnd { outcome, rewards, .. } => {
                replay.check_end(outcome, &rewards).unwrap_or_else(|d| panic!("the end diverged: {d}"));
                return Fought { party, outcome, rewards };
            }
            _ => {}
        }
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

async fn request(j: &mut Joined, formation: u16) {
    j.client.send(&ClientMsg::RequestEncounter { formation_id: Some(formation) }).await;
}

// ---------------------------------------------------------------------------------------------

/// A server that runs battles tells a player where its character stands right after the snapshot and
/// the placement; a new account starts at its character's starting level and experience. A server
/// without battle data has no characters to level and sends nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_player_is_told_where_its_character_stands_when_it_joins() {
    let server = TestServer::start(fast(None)).await;
    let mut a = join(server.addr, &uniq("Pg")).await;
    // Level 5 starts at 40 experience on the synthetic curve and level 6 at 60.
    let first = a.client.next().await;
    assert_eq!(
        progress_of(&first),
        Some(Progress { character: 0, level: 5, exp: 40, exp_to_next: 20, gil: 0, ap: 0, leveled_up: false }),
        "the third message after the handshake (Welcome ... AreaSnapshot, InstancePlacement), got {first:?}"
    );
    server.shutdown().await;

    let plain = TestServer::start(ServerConfig::default()).await;
    let mut b = join(plain.addr, &uniq("Fx")).await;
    let quiet = b.client.drain_for(Duration::from_millis(400)).await;
    assert!(quiet.iter().all(|m| progress_of(m).is_none()), "the FIXTURE server has no characters: {quiet:?}");
    plain.shutdown().await;
}

/// The whole chain on one connection: the second battle's `EncounterStart` carries the level the
/// first one gained (the replay builds the fight from it), and every message says what the player has.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_win_pays_the_player_tells_it_and_the_next_battle_is_fought_at_the_level_gained() {
    let rules = SyntheticRules::default();
    let server = TestServer::start(fast(None)).await;
    let mut a = join(server.addr, &uniq("Lv")).await;
    assert_eq!(next_progress(&mut a).await.level, 5);

    // Formation 1 is two dummies: 24 experience, 40 gil, 4 AP for the one player alive. 40 + 24 = 64
    // is level 6 (60); level 7 needs 84, which is 20 more.
    request(&mut a, 1).await;
    let first = fight(&mut a, &rules).await;
    assert_eq!(first.outcome, Outcome::Victory);
    assert_eq!((first.party[0].character, first.party[0].level), (0, 5));
    assert_eq!(first.rewards.len(), 1);
    assert_eq!((first.rewards[0].exp, first.rewards[0].gil, first.rewards[0].ap), (24, 40, 4));
    assert_eq!(
        next_progress(&mut a).await,
        Progress { character: 0, level: 6, exp: 64, exp_to_next: 20, gil: 40, ap: 4, leveled_up: true }
    );

    // The next battle is fought at level 6, which is on the wire, and pays on from 64.
    request(&mut a, 0).await;
    let second = fight(&mut a, &rules).await;
    assert_eq!((second.party[0].character, second.party[0].level), (0, 6), "the level gained is the level fought at");
    assert_eq!(
        next_progress(&mut a).await,
        Progress { character: 0, level: 6, exp: 76, exp_to_next: 8, gil: 60, ap: 6, leveled_up: false }
    );
    server.shutdown().await;
}

/// The durability the smoke cannot show (it never restarts its server): win a battle, stop the server
/// gracefully, start another on the same state directory, log in with the token, and the join
/// message says what was won. Also that stopping waits for the saver, so the store is free again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn what_a_win_paid_survives_a_restart_and_is_what_the_player_is_told_on_logging_in_again() {
    let rules = SyntheticRules::default();
    let dir = TempDir::new().unwrap();
    let state = dir.path().join("server-state");

    let server = TestServer::start(fast(Some(state.clone()))).await;
    let mut a = join(server.addr, &uniq("Rs")).await;
    let (token, account, first_placement) = (a.token.clone(), a.account, a.placement.clone());
    assert_eq!(next_progress(&mut a).await.exp, 40);
    request(&mut a, 1).await;
    assert_eq!(fight(&mut a, &rules).await.outcome, Outcome::Victory);
    let told = next_progress(&mut a).await;
    assert_eq!(told, Progress { character: 0, level: 6, exp: 64, exp_to_next: 20, gil: 40, ap: 4, leveled_up: true });
    // The battle is over and the player is told; the write may still be queued behind this moment.
    // Stopping the server saves everything queued before it returns.
    drop(a);
    server.shutdown().await;

    // A record of its own, apart from the story blob (there is none: nothing moved the phase).
    let file = state.join("progression").join(account.0.to_string()).join("Fixture.character.json");
    assert!(file.is_file(), "{file:?}");
    assert!(!state.join("progression").join(account.0.to_string()).join("Fixture.json").exists(), "the story blob was never written");

    let again = TestServer::start(fast(Some(state.clone()))).await;
    let mut a2 = join_with_token(again.addr, &token).await;
    assert_eq!(a2.account, account);
    assert_eq!(a2.placement, first_placement, "the story phase (and so the instance) is untouched by the character's progress");
    let welcome_back = next_progress(&mut a2).await;
    assert_eq!(welcome_back, Progress { leveled_up: false, ..told }, "the join message is what was saved");

    // And it goes on from there: a win at level 6 adds to the saved numbers.
    request(&mut a2, 0).await;
    let fought = fight(&mut a2, &rules).await;
    assert_eq!(fought.party[0].level, 6);
    assert_eq!(
        next_progress(&mut a2).await,
        Progress { character: 0, level: 6, exp: 76, exp_to_next: 8, gil: 60, ap: 6, leveled_up: false }
    );
    drop(a2);
    again.shutdown().await;
}

/// What a player saw of a battle it left by running.
#[derive(Debug)]
struct Ran {
    party: Vec<BattleMember>,
    outcome: Outcome,
    rewards: Vec<BattleReward>,
    limits_used: usize,
    failed_escapes: usize,
    /// The limit gauge of our character in our replay when the battle ended.
    end_gauge: u16,
}

/// Play a battle as a player who attacks until blows have filled the gauge, gives its limit, attacks until
/// a blow has filled the gauge again and then runs until it gets away. Every refusal is a failure of the
/// test: the replay says when each command is legal.
async fn limit_then_run(j: &mut Joined, rules: &SyntheticRules) -> Ran {
    let start = j.client.until("EncounterStart", |m| matches!(m, ServerMsg::EncounterStart { .. }).then(|| m.clone())).await;
    let ServerMsg::EncounterStart { battle_id, formation_id, seed, party, preemptive, back_attack, battle_speed, ticks_per_second, .. } = start
    else {
        unreachable!()
    };
    let setup = BattleSetup { formation_id, seed, party: party.clone(), preemptive, back_attack, battle_speed, ticks_per_second };
    let mut replay = BattleReplay::start(rules, &setup).expect("the client builds the battle from its own data");
    let mine = ActorId(party.iter().position(|m| m.entity == j.entity).expect("we are in the party") as u8);
    let (mut pending, mut limits_used, mut failed_escapes) = (false, 0, 0);
    loop {
        let state = replay.state();
        if !pending && state.outcome().is_none() && state.ready_actors().contains(&mine) {
            let target = state.enemy_ids().into_iter().find(|e| state.actor(*e).is_some_and(|c| c.is_alive())).expect("a living enemy");
            let command = match (limits_used, state.limit_ready(mine), state.limit_gauge(mine)) {
                (0, true, _) => Command::Limit { target },
                (0, false, _) => Command::Attack { target },
                (_, _, 0) => Command::Attack { target },
                _ => Command::Escape,
            };
            j.client.send(&ClientMsg::BattleCommand { battle_id, actor: mine, command }).await;
            pending = true;
        }
        match j.client.next().await {
            ServerMsg::BattleEvent { tick, events, command, .. } => {
                replay.apply(tick, &events, command.as_ref()).unwrap_or_else(|d| panic!("the replay diverged: {d}"));
                limits_used += events.iter().filter(|e| matches!(e, BattleEvent::Action { actor, action: ActionRef::Limit(_) } if *actor == mine)).count();
                failed_escapes += events.iter().filter(|e| matches!(e, BattleEvent::EscapeFailed { actor } if *actor == mine)).count();
                if command.is_some_and(|c| c.actor == mine) {
                    pending = false;
                }
            }
            ServerMsg::BattleRejected { reason, .. } => panic!("the server refused a command the replay allows: {reason}"),
            ServerMsg::BattleEnd { outcome, rewards, .. } => {
                replay.check_end(outcome, &rewards).unwrap_or_else(|d| panic!("the end diverged: {d}"));
                let end_gauge = replay.state().limit_gauge(mine);
                return Ran { party, outcome, rewards, limits_used, failed_escapes, end_gauge };
            }
            _ => {}
        }
    }
}

/// Protocol v7 over real sockets and a real store: a player whose blows filled its limit gauge gives the
/// limit, gets blows again and runs away; the battle ends for it in `Escaped` with nothing paid, the gauge
/// it ended with was saved (a record is made for the account, though it never won) and is the one its next
/// battle starts with, also after the server restarted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_limit_and_an_escape_over_the_wire_and_the_gauge_it_ends_with_survives_a_restart() {
    // Enemies that do not die in the time it takes (the limit alone would end a battle with 200 HP).
    let rules = SyntheticRules { enemy_hp: 1_000_000, ..SyntheticRules::default() };
    let config = |state: PathBuf| ServerConfig {
        battle: Some(BattleSettings { battle_speed: 0, ticks_per_second: 1000, ..BattleSettings::new(Arc::new(rules)) }),
        debug_encounters: true,
        state_dir: Some(state),
        ..ServerConfig::default()
    };
    let dir = TempDir::new().unwrap();
    let state = dir.path().join("server-state");

    let server = TestServer::start(config(state.clone())).await;
    let mut a = join(server.addr, &uniq("Lm")).await;
    let (token, account) = (a.token.clone(), a.account);
    assert_eq!(next_progress(&mut a).await.exp, 40);
    request(&mut a, 0).await;
    let ran = limit_then_run(&mut a, &rules).await;
    assert_eq!((ran.outcome, ran.rewards.len(), ran.limits_used), (Outcome::Escaped, 0, 1), "{ran:?}");
    assert_eq!(ran.party[0].limit, 0, "a new account starts with an empty gauge");
    assert!(ran.end_gauge > 0, "a blow filled it again after the limit emptied it: {ran:?}");
    assert!(ran.failed_escapes < 40, "a party that keeps trying gets away: {ran:?}");

    // Nothing was paid: the next thing the server says is not a progress message (no win, no level), and
    // the player is back in the field and can start another battle at once.
    request(&mut a, 0).await;
    let again = limit_then_run_start_gauge(&mut a).await;
    assert_eq!(again, ran.end_gauge, "the next battle starts with the gauge the last one ended with");
    drop(a);
    server.shutdown().await;

    // The record was made by the gauge alone: a fresh character's numbers and the gauge.
    let file = state.join("progression").join(account.0.to_string()).join("Fixture.character.json");
    let text = std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("{file:?}: {e}"));
    assert!(text.contains(&format!("\"limit\": {}", ran.end_gauge)) && text.contains("\"exp\": 40") && !text.contains("inventory"), "{text}");

    // After a restart the gauge is what the player starts its next battle with, and nothing else changed.
    let restarted = TestServer::start(config(state)).await;
    let mut a2 = join_with_token(restarted.addr, &token).await;
    let welcome = next_progress(&mut a2).await;
    assert_eq!((welcome.level, welcome.exp, welcome.gil), (5, 40, 0));
    request(&mut a2, 0).await;
    assert_eq!(limit_then_run_start_gauge(&mut a2).await, ran.end_gauge);
    drop(a2);
    restarted.shutdown().await;
}

/// The limit gauge our character starts the battle that was just announced with.
async fn limit_then_run_start_gauge(j: &mut Joined) -> u16 {
    let entity = j.entity;
    j.client
        .until("EncounterStart", |m| match m {
            ServerMsg::EncounterStart { party, .. } => party.iter().find(|p| p.entity == entity).map(|p| p.limit),
            _ => None,
        })
        .await
}

/// A party fights one battle and every member that fought on is paid into its OWN progress, told its
/// own numbers, and saved under its own account.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_member_of_a_party_is_paid_its_own_share_into_its_own_progress() {
    let rules = SyntheticRules::default();
    let dir = TempDir::new().unwrap();
    let state = dir.path().join("server-state");
    let server = TestServer::start(fast(Some(state.clone()))).await;
    let mut a = join(server.addr, &uniq("Pa")).await;
    let mut b = join(server.addr, &uniq("Pb")).await;
    let (a_token, b_token) = (a.token.clone(), b.token.clone());
    assert_eq!(next_progress(&mut a).await.exp, 40);
    assert_eq!(next_progress(&mut b).await.exp, 40);
    party_up(&mut a, &mut b).await;

    request(&mut a, 1).await;
    let (fa, fb) = tokio::join!(fight(&mut a, &rules), fight(&mut b, &rules));
    assert_eq!(fa.outcome, Outcome::Victory);
    assert_eq!(fa.party, fb.party, "one shared battle");
    assert_eq!(fa.party.len(), 2);
    // Two dummies are 24 experience, 40 gil and 4 AP; the two living members split the first two and
    // each gets the AP in full: 12, 20 and 4 each.
    let paid = |f: &Fought, who: &Joined| f.rewards.iter().find(|r| r.account == who.account).map(|r| (r.exp, r.gil, r.ap));
    assert_eq!(paid(&fa, &a), Some((12, 20, 4)));
    assert_eq!(paid(&fa, &b), Some((12, 20, 4)));
    // 40 + 12 = 52 for both: level 5 still, 8 to go.
    let want = Progress { character: 0, level: 5, exp: 52, exp_to_next: 8, gil: 20, ap: 4, leveled_up: false };
    assert_eq!(next_progress(&mut a).await, want);
    assert_eq!(next_progress(&mut b).await, want);
    drop((a, b));
    server.shutdown().await;

    // Each account has its own saved record.
    let again = TestServer::start(fast(Some(state))).await;
    for token in [a_token, b_token] {
        let mut j = join_with_token(again.addr, &token).await;
        assert_eq!(next_progress(&mut j).await, want);
    }
    again.shutdown().await;
}

/// A defeat pays nothing and tells nobody anything: no message, nothing saved.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_defeat_pays_nothing_and_sends_no_progress() {
    let rules = SyntheticRules::default();
    let dir = TempDir::new().unwrap();
    let state = dir.path().join("server-state");
    let server = TestServer::start(fast(Some(state.clone()))).await;
    let mut a = join(server.addr, &uniq("Df")).await;
    let account = a.account;
    let joined = next_progress(&mut a).await;
    request(&mut a, 2).await; // one overwhelming enemy
    let lost = fight(&mut a, &rules).await;
    assert_eq!(lost.outcome, Outcome::Defeat);
    assert!(lost.rewards.is_empty());
    let after = a.client.drain_for(Duration::from_millis(500)).await;
    assert!(after.iter().all(|m| progress_of(m).is_none()), "no progress message after a defeat: {after:?}");
    drop(a);
    server.shutdown().await;
    assert!(
        !state.join("progression").join(account.0.to_string()).join("Fixture.character.json").exists(),
        "nothing was earned, so nothing was written"
    );
    assert_eq!(joined.exp, 40);
}

/// A member that left mid-battle still fought: the others win, and what it earned is in the store
/// when it logs in again (the saver is the only way to an offline account).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_member_who_left_mid_battle_is_still_paid_in_the_store() {
    let rules = SyntheticRules::default();
    let dir = TempDir::new().unwrap();
    let state = dir.path().join("server-state");
    let server = TestServer::start(fast(Some(state.clone()))).await;
    let mut a = join(server.addr, &uniq("Ha")).await;
    let mut b = join(server.addr, &uniq("Hb")).await;
    let (b_token, b_account) = (b.token.clone(), b.account);
    party_up(&mut a, &mut b).await;
    request(&mut a, 1).await;
    b.client.until("EncounterStart", |m| matches!(m, ServerMsg::EncounterStart { .. }).then_some(())).await;
    drop(b); // B's connection ends; its character fights on, uncontrolled
    let won = fight(&mut a, &rules).await;
    assert_eq!(won.outcome, Outcome::Victory);
    let paid_b = won.rewards.iter().find(|r| r.account == b_account).expect("B is on the roster");
    assert!(paid_b.exp > 0, "B's character was alive at the end, so it is paid: {:?}", won.rewards);
    drop(a);
    server.shutdown().await;

    let again = TestServer::start(fast(Some(state))).await;
    let mut b2 = join_with_token(again.addr, &b_token).await;
    let welcome = next_progress(&mut b2).await;
    assert_eq!(welcome.exp, 40 + paid_b.exp, "what B earned while it was gone");
    assert_eq!((welcome.gil, welcome.ap), (paid_b.gil, paid_b.ap));
    again.shutdown().await;
}

/// The reason for v5, v6 and again for v7: a client of the version before is refused at the handshake,
/// with the version in the reason, instead of being sent a message it cannot decode (v5: the progress
/// that follows every join; v6: the inventory that follows it, and the new battle events; v7: the limit
/// and escape commands, events and outcome, and the limit gauge of every battle member).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_of_the_previous_protocol_version_is_refused_at_the_handshake() {
    assert_eq!(PROTOCOL_VERSION, 7);
    let server = TestServer::start(fast(None)).await;
    for old_version in [6u16, 5, 4] {
        let mut old = Client::connect(server.addr).await;
        old.send_json(&format!(
            r#"{{"Hello":{{"protocol_version":{old_version},"client_build":"old","auth":{{"Register":{{"display_name":"Old{old_version}"}}}},"tint":[1,2,3],"appearance_ref":null}}}}"#
        ))
        .await;
        old.expect_reject_as(RejectCode::ProtocolMismatch, &format!("server speaks v7, client sent v{old_version}")).await;
    }
    server.shutdown().await;
}
