//! Ordinary story and game-over holds over real TCP, with FIFO barriers instead of timed absence checks.

use super::Session;
use crate::battle::BattleCmd;
use crate::net::{NetCommand, NetConfig, NetHandle, PartyCommand, SessionTiming, TokenSink};
use fmv_adapter_api::{AreaInfo, GameAdapter};
use fmv_core::{AccountId, AnimState, AvatarAppearance, EntityId, EntitySnapshot, Pos, SessionId};
use fmv_fixture::rooms::{RoomsAdapter, HALL};
use fmv_protocol::battle::Command;
use fmv_protocol::{
    read_msg, write_msg, ActorId, Auth, BattleId, CharacterCard, ClientMsg, InviteId,
    NewCharacter, ServerMsg, PROTOCOL_VERSION,
};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpListener;

const WAIT: Duration = Duration::from_secs(10);
const HOLD: u64 = 7;
const HELD_POS: Pos = Pos::new(20.0, 30.0, 4.0);
const HELD_FACING: f32 = 0.75;

fn hall() -> AreaInfo {
    RoomsAdapter::new().list_areas().into_iter().find(|area| area.id.key == HALL).expect("the fixture hall")
}

fn config(addr: std::net::SocketAddr) -> NetConfig {
    NetConfig {
        server: addr.to_string(), auth: Auth::Register { display_name: "Hold test".into() },
        character: NewCharacter::with_tint([1, 2, 3]), client_build: "story-hold-test".into(),
        timing: SessionTiming::DEFAULT, token_sink: TokenSink::new(|_| Ok(())),
    }
}

fn me(pos: Pos) -> EntitySnapshot {
    EntitySnapshot { id: EntityId(7), pos, facing: HELD_FACING, anim: AnimState::Idle,
        avatar: AvatarAppearance::new("Hold test", [1, 2, 3], None) }
}

fn pending() -> ServerMsg {
    ServerMsg::StoryPending { id: HOLD, area: hall().id, pos: HELD_POS, facing: HELD_FACING }
}

async fn send(wr: &mut OwnedWriteHalf, message: &ServerMsg) {
    tokio::time::timeout(WAIT, write_msg(wr, message)).await.expect("server write deadline").expect("server write");
}

/// Pings are transport housekeeping; everything else stays in the assertion stream.
async fn command(rd: &mut OwnedReadHalf, wr: &mut OwnedWriteHalf) -> ClientMsg {
    loop {
        let message = tokio::time::timeout(WAIT, read_msg::<_, ClientMsg>(rd)).await
            .expect("client command deadline").expect("client command");
        if let ClientMsg::Ping { nonce } = message {
            send(wr, &ServerMsg::Pong { nonce, tick: 0 }).await;
        } else {
            return message;
        }
    }
}

async fn handshake(listener: TcpListener, before_snapshot: Vec<ServerMsg>, pos: Pos) -> (OwnedReadHalf, OwnedWriteHalf) {
    let (stream, _) = tokio::time::timeout(WAIT, listener.accept()).await.expect("accept deadline").expect("accept");
    let (mut rd, mut wr) = stream.into_split();
    assert!(matches!(command(&mut rd, &mut wr).await, ClientMsg::Hello { .. }));
    send(&mut wr, &ServerMsg::Welcome {
        protocol_version: PROTOCOL_VERSION, server_build: "fake".into(), session: SessionId(1),
        entity: EntityId(7), account: AccountId(1), issued_token: None,
        character: CharacterCard::plain("Hold test", [1, 2, 3]),
    }).await;
    let area = hall();
    send(&mut wr, &ServerMsg::AreaManifest {
        area: area.id.clone(), display_name: area.display_name, fingerprint: area.fingerprint,
        provenance: area.provenance.summary(), fixture: true,
    }).await;
    assert!(matches!(command(&mut rd, &mut wr).await, ClientMsg::JoinArea { .. }));
    for message in before_snapshot { send(&mut wr, &message).await; }
    send(&mut wr, &ServerMsg::AreaSnapshot { area: area.id, tick: 1, entities: vec![me(pos)], npcs: vec![] }).await;
    (rd, wr)
}

fn signal(tx: &Sender<&'static str>, phase: &'static str) { tx.send(phase).expect("client still waits for the phase"); }

fn wait(rx: &Receiver<&'static str>, phase: &'static str) {
    assert_eq!(rx.recv_timeout(WAIT).expect("server phase deadline"), phase);
}

fn pump_until(session: &mut Session, what: &str, mut done: impl FnMut(&Session) -> bool) {
    let deadline = Instant::now() + WAIT;
    while !done(session) {
        assert!(session.pump().is_none(), "the session closed while waiting for {what}");
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Exercise the four field-action send paths and every mutating party request.
fn attempt_held_actions(session: &mut Session) {
    session.state.own.set_motion(Pos::new(100.0, 200.0, 300.0), 2.0, AnimState::Run);
    session.flush_intent();
    session.send_interact(Some(4));
    session.send_battle(BattleCmd::RequestEncounter { formation: Some(303) });
    session.state.scene.request_preview("test/hold/preview");
    session.flush_scene();
    for command in [PartyCommand::Invite(EntityId(9)), PartyCommand::Accept(InviteId(1)),
        PartyCommand::Decline(InviteId(1)), PartyCommand::Leave, PartyCommand::Kick(EntityId(9)),
        PartyCommand::Promote(EntityId(9))] {
        session.send_party(command);
    }
}

fn battle_command() -> BattleCmd {
    BattleCmd::Command { battle_id: BattleId(21), actor: ActorId(0), command: Command::Attack { target: ActorId(1) } }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn story_pending_ack_follows_installed_hold_and_field_actions_wait_for_matching_commit() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (phase_tx, phase_rx) = mpsc::channel();
    let server = tokio::spawn(async move {
        let (mut rd, mut wr) = handshake(listener, vec![], Pos::new(-100.0, 0.0, 0.0)).await;
        send(&mut wr, &pending()).await;
        signal(&phase_tx, "pending sent");
        // The client deliberately has not pumped. Its already-issued field actions stay in
        // connection order before the ACK; receipt in the network thread is not an ACK.
        assert_eq!(command(&mut rd, &mut wr).await, ClientMsg::MoveIntent {
            seq: 1, pos: Pos::new(-90.0, 0.0, 0.0), facing: 1.0, anim: AnimState::Run,
        });
        assert_eq!(command(&mut rd, &mut wr).await, ClientMsg::Interact { npc: Some(4) });
        assert_eq!(command(&mut rd, &mut wr).await, ClientMsg::PartySync);
        signal(&phase_tx, "before pump verified");
        assert_eq!(command(&mut rd, &mut wr).await, ClientMsg::StoryPendingAck { id: HOLD });
        assert_eq!(command(&mut rd, &mut wr).await, battle_command().to_msg(), "ongoing combat remains operable while field actions are held");
        assert_eq!(command(&mut rd, &mut wr).await, ClientMsg::PartySync, "no field action or party mutation may precede the FIFO barrier");
        signal(&phase_tx, "held actions verified");
        send(&mut wr, &ServerMsg::StoryCommitted { id: HOLD - 1 }).await;
        send(&mut wr, &ServerMsg::StoryUpdate { progress: vec![5], advance: None }).await;
        assert_eq!(command(&mut rd, &mut wr).await, ClientMsg::PartySync, "a stale commit cannot admit any queued field action");
        signal(&phase_tx, "stale commit verified");
        send(&mut wr, &ServerMsg::StoryCommitted { id: HOLD }).await;
        assert_eq!(command(&mut rd, &mut wr).await, ClientMsg::MoveIntent {
            seq: 2, pos: Pos::new(24.0, 30.0, 4.0), facing: 0.25, anim: AnimState::Walk,
        });
        assert_eq!(command(&mut rd, &mut wr).await, ClientMsg::Interact { npc: Some(4) });
        assert_eq!(command(&mut rd, &mut wr).await, ClientMsg::RequestEncounter { formation_id: Some(303) });
        assert_eq!(command(&mut rd, &mut wr).await, ClientMsg::SceneDebugPreview { scene: "test/hold/preview".into() });
        assert_eq!(command(&mut rd, &mut wr).await, ClientMsg::PartyInvite { target: EntityId(9) });
        assert_eq!(command(&mut rd, &mut wr).await, ClientMsg::PartySync);
        signal(&phase_tx, "released actions verified");
        assert_eq!(command(&mut rd, &mut wr).await, ClientMsg::Leave);
    });
    let client = tokio::task::spawn_blocking(move || {
        let mut net = NetHandle::spawn(config(addr), Box::new(RoomsAdapter::new())).expect("network thread");
        let joined = net.wait_joined().expect("joined");
        let mut session = Session::new(net, joined);
        session.state.own.set_motion(Pos::new(-90.0, 0.0, 0.0), 1.0, AnimState::Run);
        assert!(session.state.own.has_unsent());
        wait(&phase_rx, "pending sent");
        session.flush_intent();
        assert!(!session.state.own.has_unsent(), "pre-hold movement is sent before this connection acknowledges the hold");
        session.send_interact(Some(4));
        session.send_party(PartyCommand::Sync);
        wait(&phase_rx, "before pump verified");
        pump_until(&mut session, "the authoritative hold", |s| s.state.story_pending_id() == Some(HOLD));
        assert_eq!((session.state.own.pos(), session.state.own.facing(), session.state.own.anim()), (HELD_POS, HELD_FACING, AnimState::Idle));
        assert!(!session.state.own.has_unsent());
        assert_eq!(session.state.story_pending_epoch(), 1);
        attempt_held_actions(&mut session);
        assert_eq!(session.state.own.pos(), HELD_POS, "prediction is held too");
        session.send_battle(battle_command());
        session.send_party(PartyCommand::Sync);
        wait(&phase_rx, "held actions verified");
        pump_until(&mut session, "the stale commit's ordered marker", |s| s.state.story.progress() == [5]);
        assert_eq!(session.state.story_pending_id(), Some(HOLD));
        attempt_held_actions(&mut session);
        session.send_party(PartyCommand::Sync);
        wait(&phase_rx, "stale commit verified");
        pump_until(&mut session, "the matching commit", |s| !s.state.is_story_pending());
        assert_eq!(session.state.story_pending_epoch(), 1);
        assert_eq!((session.state.own.pos(), session.state.own.facing(), session.state.own.anim()), (HELD_POS, HELD_FACING, AnimState::Idle));
        session.state.own.set_motion(Pos::new(24.0, 30.0, 4.0), 0.25, AnimState::Walk);
        let movement_deadline = Instant::now() + WAIT;
        while session.state.own.has_unsent() {
            session.flush_intent();
            assert!(Instant::now() < movement_deadline, "post-commit movement must be sent once the preserved rate clock permits it");
            if session.state.own.has_unsent() { std::thread::sleep(Duration::from_millis(2)); }
        }
        session.send_interact(Some(4));
        session.send_battle(BattleCmd::RequestEncounter { formation: Some(303) });
        session.state.scene.request_preview("test/hold/preview");
        session.flush_scene();
        session.send_party(PartyCommand::Invite(EntityId(9)));
        session.send_party(PartyCommand::Sync);
        wait(&phase_rx, "released actions verified");
        session.leave(Duration::from_secs(2));
    });
    client.await.expect("client task");
    server.await.expect("server task");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn handshake_story_hold_and_same_batch_commit_ack_once_and_preserve_the_hold_epoch() {
    for commit_in_backlog in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (phase_tx, phase_rx) = mpsc::channel();
        let server = tokio::spawn(async move {
            let mut prefix = vec![pending()];
            if commit_in_backlog { prefix.push(ServerMsg::StoryCommitted { id: HOLD }); }
            let (mut rd, mut wr) = handshake(listener, prefix, HELD_POS).await;
            // The network handshake decoded the snapshot, but no Session has installed the hold.
            assert_eq!(command(&mut rd, &mut wr).await, ClientMsg::PartySync);
            signal(&phase_tx, "construct session");
            assert_eq!(command(&mut rd, &mut wr).await, ClientMsg::StoryPendingAck { id: HOLD });
            assert_eq!(command(&mut rd, &mut wr).await, ClientMsg::PartySync, "the constructor/first pump must not duplicate its ACK");
            send(&mut wr, &ServerMsg::StoryCommitted { id: HOLD }).await;
            send(&mut wr, &ServerMsg::StoryUpdate { progress: vec![9], advance: None }).await;
            assert_eq!(command(&mut rd, &mut wr).await, ClientMsg::PartySync, "a commit, including a duplicate, never generates another ACK");
            signal(&phase_tx, "committed epoch verified");
            assert_eq!(command(&mut rd, &mut wr).await, ClientMsg::Leave);
        });
        let client = tokio::task::spawn_blocking(move || {
            let mut net = NetHandle::spawn(config(addr), Box::new(RoomsAdapter::new())).expect("network thread");
            let joined = net.wait_joined().expect("joined");
            assert!(joined.backlog.iter().any(|m| matches!(m, ServerMsg::StoryPending { id: HOLD, .. })), "the ordered hold survived the handshake");
            net.send(NetCommand::Party(PartyCommand::Sync));
            wait(&phase_rx, "construct session");
            let mut session = Session::new(net, joined);
            assert_eq!(session.state.is_story_pending(), !commit_in_backlog);
            assert_eq!(session.state.story_pending_epoch(), 1, "same-batch release must still retire pre-hold input");
            assert_eq!((session.state.own.pos(), session.state.own.facing(), session.state.own.anim()), (HELD_POS, HELD_FACING, AnimState::Idle));
            assert!(!session.state.own.has_unsent());
            assert!(session.pump().is_none());
            session.send_party(PartyCommand::Sync);
            pump_until(&mut session, "the post-commit marker", |s| s.state.story.progress() == [9]);
            assert!(!session.state.is_story_pending());
            assert_eq!(session.state.story_pending_epoch(), 1);
            assert!(session.pump().is_none());
            session.send_party(PartyCommand::Sync);
            wait(&phase_rx, "committed epoch verified");
            session.leave(Duration::from_secs(2));
        });
        client.await.expect("client task");
        server.await.expect("server task");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn a_fatal_handshake_story_hold_stops_the_backlog_before_any_later_acknowledgement() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (phase_tx, phase_rx) = mpsc::channel();
    let server = tokio::spawn(async move {
        let invalid = ServerMsg::StoryPending { id: 0, area: hall().id, pos: HELD_POS, facing: HELD_FACING };
        let (mut rd, mut wr) = handshake(listener, vec![invalid, pending()], HELD_POS).await;
        assert_eq!(command(&mut rd, &mut wr).await, ClientMsg::PartySync);
        signal(&phase_tx, "construct rejected session");
        assert_eq!(command(&mut rd, &mut wr).await, ClientMsg::PartySync,
            "neither the invalid hold nor a valid hold after the fatal error may be acknowledged");
        signal(&phase_tx, "fatal backlog verified");
        assert_eq!(command(&mut rd, &mut wr).await, ClientMsg::Leave);
    });
    let client = tokio::task::spawn_blocking(move || {
        let mut net = NetHandle::spawn(config(addr), Box::new(RoomsAdapter::new())).expect("network thread");
        let joined = net.wait_joined().expect("wire-valid messages survive the handshake for state validation");
        net.send(NetCommand::Party(PartyCommand::Sync));
        wait(&phase_rx, "construct rejected session");
        let mut session = Session::new(net, joined);
        assert_eq!(session.state.story_pending_epoch(), 0, "the later valid hold was never installed");
        assert!(!session.state.is_story_pending());
        assert!(session.pump().is_some_and(|reason| reason.contains("zero id")));
        session.send_party(PartyCommand::Sync);
        wait(&phase_rx, "fatal backlog verified");
        session.leave(Duration::from_secs(2));
    });
    client.await.expect("client task");
    server.await.expect("server task");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn ordinary_game_over_keeps_field_commands_held_after_the_result_expires_but_debug_defeat_does_not() {
    for game_over_on_defeat in [true, false] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (phase_tx, phase_rx) = mpsc::channel();
        let server = tokio::spawn(async move {
            let (mut rd, mut wr) = handshake(listener, vec![], HELD_POS).await;
            send(&mut wr, &ServerMsg::EncounterStart {
                battle_id: BattleId(21), formation_id: 1, seed: 7,
                party: vec![fmv_protocol::BattleMember::player(EntityId(7), AccountId(1), "Hold test", 0, 6, 0)],
                initial_party: None, escape_policy: fmv_protocol::EscapePolicy::Formation,
                victory_fanfare: true, game_over_on_defeat,
                preemptive: false, back_attack: false, battle_speed: 0, ticks_per_second: 1000,
                data_fingerprint: fmv_protocol::SyntheticRules::FINGERPRINT,
            }).await;
            assert_eq!(command(&mut rd, &mut wr).await, battle_command().to_msg(), "the ongoing fight remains operable");
            send(&mut wr, &ServerMsg::BattleEnd {
                battle_id: BattleId(21), outcome: fmv_protocol::battle::Outcome::Defeat, rewards: vec![],
            }).await;
            let mut actions = Vec::new();
            loop {
                let action = command(&mut rd, &mut wr).await;
                if action == ClientMsg::PartySync { break; }
                actions.push(action);
            }
            let expected = if game_over_on_defeat { vec![] } else { vec![
                ClientMsg::MoveIntent { seq: 1, pos: Pos::new(100.0, 200.0, 300.0), facing: 2.0, anim: AnimState::Run },
                ClientMsg::Interact { npc: Some(4) },
                ClientMsg::RequestEncounter { formation_id: Some(303) },
                ClientMsg::SceneDebugPreview { scene: "test/hold/preview".into() },
                ClientMsg::PartyInvite { target: EntityId(9) },
                ClientMsg::PartyAccept { invite: InviteId(1) },
                ClientMsg::PartyDecline { invite: InviteId(1) },
                ClientMsg::PartyLeave,
                ClientMsg::PartyKick { target: EntityId(9) },
                ClientMsg::PartyPromote { target: EntityId(9) },
            ] };
            assert_eq!(actions, expected, "the explicit defeat policy governs every field-action path after the panel expires");
            signal(&phase_tx, "post-defeat actions verified");
            assert_eq!(command(&mut rd, &mut wr).await, ClientMsg::Leave, "disconnect remains available during Game Over");
        });
        let client = tokio::task::spawn_blocking(move || {
            let mut net = NetHandle::spawn(config(addr), Box::new(RoomsAdapter::new())).expect("network thread");
            let joined = net.wait_joined().expect("joined");
            let mut session = Session::new(net, joined);
            session.state.battle.set_rules(None);
            pump_until(&mut session, "the encounter", |s| s.state.battle.in_battle());
            session.send_battle(battle_command());
            pump_until(&mut session, "the authoritative defeat", |s| s.state.battle.ended() == 1);
            assert!(!session.state.is_story_pending(), "this hold must work without a story transaction token");
            assert_eq!(session.state.ordinary_game_over(), game_over_on_defeat);
            // Advance the session's clock without waiting in real time. The result panel is
            // gone, so a lingering-battle gate cannot accidentally make the assertion pass.
            session.origin = Instant::now() - Duration::from_secs(10_000);
            assert!(!session.state.battle.lingering(session.now()));
            assert!(session.state.battle.view(session.now()).is_none());
            attempt_held_actions(&mut session);
            if game_over_on_defeat {
                session.send_battle(battle_command()); // even a direct stale command cannot leave terminal state
            }
            assert_eq!(session.state.own.pos(), if game_over_on_defeat { HELD_POS } else { Pos::new(100.0, 200.0, 300.0) });
            assert!(!session.state.own.has_unsent(), "held prediction produces no intent; unheld movement was flushed");
            session.send_party(PartyCommand::Sync);
            wait(&phase_rx, "post-defeat actions verified");
            session.leave(Duration::from_secs(2));
        });
        client.await.expect("client task");
        server.await.expect("server task");
    }
}
