//! Tests of the battle mirror against a simulated server (`fmv_protocol::SyntheticRules` plays the
//! game's rules; no game files are involved).

use super::*;
use fmv_core::EntityId;
use fmv_ff7_battle::rules::{BattleState, GAUGE_MAX};
use fmv_protocol::{split_rewards, BattleMember, SyntheticRules};

const ME: AccountId = AccountId(11);
const PEER: AccountId = AccountId(12);

fn rules() -> Arc<dyn BattleRules> {
    Arc::new(SyntheticRules::default())
}

fn client(with_rules: bool) -> BattleClient {
    let mut c = BattleClient::new(ME);
    c.set_rules(with_rules.then(rules));
    c
}

fn members(accounts: &[AccountId]) -> Vec<BattleMember> {
    accounts
        .iter()
        .enumerate()
        .map(|(i, &account)| BattleMember {
            entity: EntityId(100 + i as u64),
            account,
            character: i as u8,
            level: 6,
            weapon: None,
            armor: None,
            limit: 0,
        })
        .collect()
}

/// A server for one battle: builds the engine like the real one and produces the messages.
struct Sim {
    id: BattleId,
    setup: BattleSetup,
    state: BattleState,
}

impl Sim {
    fn new(formation: u16, accounts: &[AccountId]) -> Self {
        let setup = BattleSetup {
            formation_id: formation,
            seed: 99,
            party: members(accounts),
            preemptive: false,
            back_attack: false,
            battle_speed: 0,
            ticks_per_second: 1000,
        };
        let state = SyntheticRules::default().build(&setup).unwrap();
        Sim { id: BattleId(1), setup, state }
    }

    fn start(&self) -> ServerMsg {
        ServerMsg::EncounterStart {
            battle_id: self.id,
            formation_id: self.setup.formation_id,
            seed: self.setup.seed,
            party: self.setup.party.clone(),
            preemptive: self.setup.preemptive,
            back_attack: self.setup.back_attack,
            battle_speed: self.setup.battle_speed,
            ticks_per_second: self.setup.ticks_per_second,
            data_fingerprint: SyntheticRules::FINGERPRINT,
        }
    }

    /// Advance; the message only if something happened (as the server does).
    fn step(&mut self, ticks: u64) -> Option<ServerMsg> {
        let events = self.state.step_ticks(ticks);
        (!events.is_empty()).then(|| ServerMsg::BattleEvent { battle_id: self.id, tick: self.state.tick(), events, command: None })
    }

    fn attack(&mut self, actor: ActorId) -> Option<ServerMsg> {
        let target = self.state.enemy_ids().into_iter().find(|e| self.state.actor(*e).is_some_and(|c| c.is_alive()))?;
        let command = Command::Attack { target };
        let events = self.state.submit_command(actor, command).ok()?;
        Some(ServerMsg::BattleEvent {
            battle_id: self.id,
            tick: self.state.tick(),
            events,
            command: Some(AppliedCommand { actor, command }),
        })
    }

    fn end(&self) -> ServerMsg {
        let outcome = self.state.outcome().expect("over");
        let roster: Vec<(AccountId, bool)> = self
            .setup
            .party
            .iter()
            .enumerate()
            .map(|(i, m)| (m.account, self.state.actor(ActorId(i as u8)).unwrap().is_alive()))
            .collect();
        let rewards = if outcome == Outcome::Victory { split_rewards(self.state.rewards(), &roster) } else { vec![] };
        ServerMsg::BattleEnd { battle_id: self.id, outcome, rewards }
    }

    /// Play the whole battle with every ready party member attacking, feeding each message to
    /// every client in `clients` as it is produced. Returns the number of messages.
    fn play(&mut self, clients: &mut [&mut BattleClient]) -> usize {
        let mut sent = 0;
        let mut t = 0.0;
        for _ in 0..100_000 {
            if self.state.outcome().is_some() {
                break;
            }
            t += 0.005;
            let mut batch: Vec<ServerMsg> = self.step(5).into_iter().collect();
            for id in self.state.ready_actors() {
                batch.extend(self.attack(id));
            }
            for msg in batch {
                sent += 1;
                for c in clients.iter_mut() {
                    c.on_message(t, msg.clone());
                }
            }
        }
        for c in clients.iter_mut() {
            c.on_message(t, self.end());
        }
        sent + 1
    }
}

fn notes(c: &mut BattleClient) -> String {
    c.drain_notes().join("\n")
}

#[test]
fn a_start_builds_the_mirror_and_says_who_we_are() {
    let sim = Sim::new(1, &[PEER, ME]);
    let mut c = client(true);
    assert!(!c.in_battle());
    c.on_message(1.0, sim.start());
    assert!(c.in_battle());
    assert_eq!(c.entered(), 1);
    let a = c.active().unwrap();
    assert_eq!((a.id(), a.my_actor()), (BattleId(1), Some(ActorId(1))), "we are the second member");
    assert_eq!(a.replay().map(|r| r.party().len()), Some(2));
    assert!(!a.diverged() && a.messages() == 0);
    let n = notes(&mut c);
    assert!(n.contains("battle#1 started: formation 1, party of 2") && n.contains("you fight as actor 1"), "{n}");
    assert_eq!(c.divergences(), 0);
}

#[test]
fn a_whole_battle_replays_without_divergence_on_every_client_and_counts_a_victory() {
    for accounts in [vec![ME], vec![ME, PEER], vec![PEER, ME]] {
        let mut sim = Sim::new(1, &accounts);
        let (mut a, mut b) = (client(true), client(true));
        // `b` is somebody else's client: it is not in the party at all, but still mirrors.
        b.me = AccountId(999);
        a.on_message(0.0, sim.start());
        b.on_message(0.0, sim.start());
        let sent = sim.play(&mut [&mut a, &mut b]);
        assert!(sent > 3);
        for c in [&mut a, &mut b] {
            assert!(!c.in_battle(), "back in the field");
            assert_eq!((c.entered(), c.ended(), c.victories(), c.defeats(), c.divergences()), (1, 1, 1, 0, 0));
            assert_eq!(c.verified(), 1, "replayed from this machine's own data and matched, ending included");
            let n = notes(c);
            assert!(!n.contains("DIVERGENCE"), "{n}");
            assert!(n.contains("ended in Victory") && n.contains("back in the field"), "{n}");
            assert!(n.contains("verified: the server's") && !n.contains("NOT verified"), "{n}");
        }
        assert!(notes(&mut a).is_empty(), "notes are drained once");
        assert_eq!(b.active().map(ActiveBattle::id), None);
        // The party member's own commands were applied (and counted from the server's echo); the
        // spectator, who is nobody's actor, has none.
        assert!(a.commands_applied() >= 1, "our character attacked");
        assert_eq!(b.commands_applied(), 0);
        assert!(a.summary().contains("1 verified by replay") && a.summary().contains("0 divergence(s)"), "{}", a.summary());
    }
}

#[test]
fn the_end_note_counts_our_commands_and_says_how_it_was_verified() {
    let mut sim = Sim::new(1, &[PEER, ME]);
    let mut c = client(true);
    c.on_message(0.0, sim.start());
    sim.play(&mut [&mut c]);
    let n = notes(&mut c);
    let mine = c.commands_applied();
    assert!(mine >= 1);
    assert!(n.contains(&format!("{mine} command(s) of yours applied")), "{n}");
    assert!(n.contains("message(s) replayed from this machine's own battle data and matched"), "{n}");
}

#[test]
fn a_lost_battle_counts_a_defeat_and_pays_nothing() {
    let mut sim = Sim::new(2, &[ME, PEER]);
    let mut c = client(true);
    c.on_message(0.0, sim.start());
    sim.play(&mut [&mut c]);
    assert_eq!((c.entered(), c.victories(), c.defeats(), c.divergences()), (1, 0, 1, 0));
    assert_eq!(c.verified(), 1, "a defeat is replayed and checked like a victory");
    let n = notes(&mut c);
    assert!(n.contains("ended in Defeat") && n.contains("no reward for you"), "{n}");
}

#[test]
fn our_reward_is_named_in_the_note() {
    let mut sim = Sim::new(0, &[ME]);
    let mut c = client(true);
    c.on_message(0.0, sim.start());
    sim.play(&mut [&mut c]);
    let n = notes(&mut c);
    assert!(n.contains("you earned 12 exp, 20 gil, 2 ap"), "{n}");
}

/// The replay catching a forged event: counted, loud, flagged, and the battle goes on.
#[test]
fn a_tampered_event_is_a_loud_divergence_and_the_mirror_keeps_going() {
    let mut sim = Sim::new(1, &[ME]);
    let mut c = client(true);
    c.on_message(0.0, sim.start());
    let mut t = 0.0;
    let mut tampered = false;
    while sim.state.outcome().is_none() {
        t += 0.005;
        let mut batch: Vec<ServerMsg> = sim.step(5).into_iter().collect();
        for id in sim.state.ready_actors() {
            batch.extend(sim.attack(id));
        }
        for mut msg in batch {
            if let (false, ServerMsg::BattleEvent { events, .. }) = (tampered, &mut msg) {
                if let Some(BattleEvent::Damage { amount, .. }) = events.iter_mut().find(|e| matches!(e, BattleEvent::Damage { .. })) {
                    *amount += 1;
                    tampered = true;
                }
            }
            c.on_message(t, msg);
        }
    }
    assert!(tampered);
    c.on_message(t, sim.end());
    assert!(c.divergences() >= 1);
    let n = notes(&mut c);
    assert!(n.contains("BATTLE DIVERGENCE") && n.contains("events differ"), "{n}");
    assert!(n.contains("NOT verified: the replay diverged"), "{n}");
    assert_eq!(c.verified(), 0, "a battle that diverged is never counted as verified");
    assert!(!c.in_battle(), "the end still ends it");
    assert_eq!(c.victories(), 1, "the server's word on the outcome is counted");
}

#[test]
fn the_active_battle_is_flagged_and_the_view_goes_red() {
    let mut sim = Sim::new(1, &[ME]);
    let mut c = client(true);
    c.on_message(0.0, sim.start());
    assert_eq!(c.view(0.0).unwrap().status, Status::Verified);
    // A message for a tick that does not follow.
    let forged = ServerMsg::BattleEvent { battle_id: BattleId(1), tick: 0, events: vec![BattleEvent::Victory], command: None };
    c.on_message(0.1, forged);
    assert_eq!(c.divergences(), 1);
    assert!(c.active().unwrap().diverged());
    assert_eq!(c.view(0.1).unwrap().status, Status::Diverged);
    assert!(c.title_suffix(0.1).contains("DIVERGED"));
    let _ = sim.step(1);
}

#[test]
fn a_different_battle_data_fingerprint_is_reported_at_once() {
    let sim = Sim::new(0, &[ME]);
    let mut start = sim.start();
    if let ServerMsg::EncounterStart { data_fingerprint, .. } = &mut start {
        *data_fingerprint = Fingerprint([1; 32]);
    }
    let mut c = client(true);
    c.on_message(0.0, start);
    assert_eq!(c.divergences(), 1);
    assert!(c.active().unwrap().diverged());
    let n = notes(&mut c);
    assert!(n.contains("BATTLE DIVERGENCE") && n.contains("battle data differs"), "{n}");
}

#[test]
fn a_battle_this_machine_cannot_build_is_reported_not_hidden() {
    // The server names a formation these rules do not know.
    let sim = Sim::new(0, &[ME]);
    let mut start = sim.start();
    if let ServerMsg::EncounterStart { formation_id, .. } = &mut start {
        *formation_id = 99;
    }
    let mut c = client(true);
    c.on_message(0.0, start);
    assert!(c.in_battle());
    assert_eq!(c.divergences(), 1);
    assert!(c.active().unwrap().replay().is_none());
    assert!(notes(&mut c).contains("cannot rebuild the battle"));
    // Without a replay it neither judges messages nor attacks, and never panics.
    c.on_message(0.1, ServerMsg::BattleEvent { battle_id: BattleId(1), tick: 5, events: vec![], command: None });
    assert_eq!(c.attack_command(0.1), None);
    assert_eq!(c.view(0.1).unwrap().status, Status::Unverified);
    c.on_message(0.2, ServerMsg::BattleEnd { battle_id: BattleId(1), outcome: Outcome::Victory, rewards: vec![] });
    assert!(!c.in_battle());
    assert_eq!(c.verified(), 0, "a battle that could not be rebuilt here was not checked");
    assert!(notes(&mut c).contains("NOT verified: the replay diverged or could not be made"));
}

#[test]
fn a_machine_without_battle_data_says_so_and_cannot_play() {
    let mut sim = Sim::new(0, &[ME]);
    let mut c = client(false);
    c.on_message(0.0, sim.start());
    assert!(c.in_battle(), "the server froze us all the same");
    assert_eq!(c.divergences(), 0, "nothing to compare is not a divergence");
    let n = notes(&mut c);
    assert!(n.contains("no battle data"), "{n}");
    for _ in 0..50 {
        if let Some(m) = sim.step(20) {
            c.on_message(0.5, m);
        }
    }
    assert_eq!(c.attack_command(1.0), None);
    let v = c.view(1.0).unwrap();
    assert_eq!((v.status, v.party.len(), v.enemies.len()), (Status::Unverified, 0, 0));
    assert!(c.title_suffix(1.0).contains("no local battle data"));
    // The battle ends all the same, but it was never checked here and is not counted as verified.
    c.on_message(2.0, ServerMsg::BattleEnd { battle_id: BattleId(1), outcome: Outcome::Victory, rewards: vec![] });
    assert_eq!((c.ended(), c.victories(), c.verified(), c.divergences()), (1, 1, 0, 0));
    let n = notes(&mut c);
    assert!(n.contains("NOT verified: this machine has no battle data") && !n.contains("DIVERGENCE"), "{n}");
    assert!(c.summary().contains("0 verified by replay"), "{}", c.summary());
}

#[test]
fn we_attack_only_when_our_own_gauge_is_full_the_first_living_enemy_and_once() {
    let mut sim = Sim::new(1, &[PEER, ME]);
    let mut c = client(true);
    c.on_message(0.0, sim.start());
    assert_eq!(c.attack_command(0.0), None, "nothing is ready at the start");
    // Advance the server until OUR actor (index 1) is ready; the peer's readiness is not ours.
    let mut now = 0.0;
    let mut peer_ready_first = false;
    for _ in 0..10_000 {
        let Some(m) = sim.step(1) else { continue };
        now += 0.001;
        c.on_message(now, m);
        let ready = sim.state.ready_actors();
        if ready.contains(&ActorId(0)) && !ready.contains(&ActorId(1)) {
            peer_ready_first = true;
            assert_eq!(c.attack_command(now), None, "only the peer is ready: not our turn");
        }
        if ready.contains(&ActorId(1)) {
            break;
        }
    }
    assert!(sim.state.ready_actors().contains(&ActorId(1)), "our gauge fills");
    let _ = peer_ready_first;
    let cmd = c.attack_command(now).expect("our turn");
    match cmd {
        BattleCmd::Command { battle_id, actor, command: Command::Attack { target } } => {
            assert_eq!((battle_id, actor), (BattleId(1), ActorId(1)));
            assert_eq!(target, ActorId(2), "the first enemy: after the two party members");
        }
        other => panic!("{other:?}"),
    }
    // Sending it arms the guard: no second command until the answer (or the timeout).
    c.note_sent(&cmd, now);
    assert_eq!(c.attack_command(now), None);
    assert_eq!(c.attack_command(now + PENDING_TIMEOUT_S - 0.01), None);
    assert!(c.attack_command(now + PENDING_TIMEOUT_S).is_some(), "a lost command may be sent again");
    // The server's answer clears the guard (and a new command waits for a new full gauge).
    let answer = sim.attack(ActorId(1)).expect("the server applies it");
    c.on_message(now + 0.01, answer);
    assert_eq!(c.attack_command(now + 0.02), None, "the gauge was spent");
}

#[test]
fn a_refusal_clears_the_guard_and_is_counted() {
    let mut sim = Sim::new(0, &[ME]);
    let mut c = client(true);
    c.on_message(0.0, sim.start());
    let mut now = 0.0;
    while c.attack_command(now).is_none() {
        now += 0.001;
        if let Some(m) = sim.step(1) {
            c.on_message(now, m);
        }
        assert!(now < 60.0, "our gauge fills");
    }
    let cmd = c.attack_command(now).unwrap();
    c.note_sent(&cmd, now);
    assert_eq!(c.attack_command(now), None);
    c.on_message(now, ServerMsg::BattleRejected { battle_id: Some(BattleId(1)), reason: BattleRejection::GaugeNotFull });
    assert_eq!((c.rejections(), c.last_rejection()), (1, Some(BattleRejection::GaugeNotFull)));
    assert!(c.attack_command(now).is_some(), "the refusal answered it");
    assert!(notes(&mut c).contains("request refused: that character's ATB gauge is not full"));
    // A refusal that is about no battle of ours still counts and does not disturb anything.
    c.on_message(now, ServerMsg::BattleRejected { battle_id: None, reason: BattleRejection::Unsupported });
    assert_eq!(c.rejections(), 2);
    assert!(c.in_battle());
    assert!(c.summary().contains("2 refusal(s)"), "{}", c.summary());
}

#[test]
fn a_member_who_is_not_us_gives_us_no_character_to_command() {
    let mut sim = Sim::new(0, &[PEER]);
    let mut c = client(true);
    c.on_message(0.0, sim.start());
    assert_eq!(c.active().unwrap().my_actor(), None);
    assert!(notes(&mut c).contains("you are not in its party"));
    for _ in 0..200 {
        if let Some(m) = sim.step(5) {
            c.on_message(0.1, m);
        }
    }
    assert_eq!(c.attack_command(1.0), None);
    assert!(c.title_suffix(1.0).contains("watching"));
}

#[test]
fn messages_for_other_battles_are_ignored_and_a_new_start_replaces_the_old_battle() {
    let sim = Sim::new(0, &[ME]);
    let mut c = client(true);
    // Nothing active: stray messages are noted and ignored.
    c.on_message(0.0, ServerMsg::BattleEvent { battle_id: BattleId(5), tick: 1, events: vec![], command: None });
    c.on_message(0.0, ServerMsg::BattleEnd { battle_id: BattleId(5), outcome: Outcome::Victory, rewards: vec![] });
    assert_eq!((c.entered(), c.ended(), c.victories(), c.divergences()), (0, 0, 0, 0));
    assert_eq!(notes(&mut c).matches("not the battle we are in").count(), 2);
    // A battle runs; an end for ANOTHER battle does not end it.
    c.on_message(1.0, sim.start());
    c.on_message(1.0, ServerMsg::BattleEnd { battle_id: BattleId(9), outcome: Outcome::Defeat, rewards: vec![] });
    assert!(c.in_battle());
    assert_eq!(c.defeats(), 0);
    // A second start while one runs drops the first (a server that does that is wrong, but we
    // must not keep a ghost).
    let mut second = Sim::new(0, &[ME]);
    second.id = BattleId(2);
    c.on_message(2.0, second.start());
    assert_eq!(c.active().unwrap().id(), BattleId(2));
    assert_eq!(c.entered(), 2);
    assert!(notes(&mut c).contains("was still running"));
}

#[test]
fn an_end_before_the_engine_finished_is_a_divergence() {
    let sim = Sim::new(0, &[ME]);
    let mut c = client(true);
    c.on_message(0.0, sim.start());
    let _ = notes(&mut c);
    c.on_message(0.1, ServerMsg::BattleEnd { battle_id: BattleId(1), outcome: Outcome::Victory, rewards: vec![] });
    assert_eq!(c.divergences(), 1);
    assert!(notes(&mut c).contains("BATTLE DIVERGENCE"));
    assert!(!c.in_battle());
}

#[test]
fn wrong_rewards_in_the_end_are_a_divergence() {
    let mut sim = Sim::new(0, &[ME]);
    let mut c = client(true);
    c.on_message(0.0, sim.start());
    let mut t = 0.0;
    while sim.state.outcome().is_none() {
        t += 0.005;
        let mut batch: Vec<ServerMsg> = sim.step(5).into_iter().collect();
        for id in sim.state.ready_actors() {
            batch.extend(sim.attack(id));
        }
        for m in batch {
            c.on_message(t, m);
        }
    }
    let mut end = sim.end();
    if let ServerMsg::BattleEnd { rewards, .. } = &mut end {
        rewards[0].gil += 1000;
    }
    c.on_message(t, end);
    assert_eq!(c.divergences(), 1);
    assert!(notes(&mut c).contains("rewards differ"));
}

#[test]
fn only_the_first_divergences_are_written_out_and_the_rest_counted() {
    let sim = Sim::new(0, &[ME]);
    let mut c = client(true);
    c.on_message(0.0, sim.start());
    let _ = notes(&mut c);
    for i in 0..20u64 {
        c.on_message(1.0, ServerMsg::BattleEvent { battle_id: BattleId(1), tick: 0, events: vec![BattleEvent::Death { actor: ActorId(0) }], command: None });
        let _ = i;
    }
    assert_eq!(c.divergences(), 20);
    let n = notes(&mut c);
    assert_eq!(n.matches("BATTLE DIVERGENCE").count(), 6, "five in full and one saying the rest are counted: {n}");
    assert!(n.contains("only counted"));
}

#[test]
fn the_view_extrapolates_gauges_between_messages_and_snaps_back_at_the_next() {
    let mut sim = Sim::new(0, &[ME]);
    let mut c = client(true);
    c.on_message(0.0, sim.start());
    let at = |c: &BattleClient, t: f64| {
        let v = c.view(t).unwrap();
        (v.party[0].gauge, v.party[0].ready)
    };
    let (g0, ready0) = at(&c, 0.0);
    assert!(!ready0);
    // 20 ms later (20 engine ticks at 1000 per second), still no message: the bar has moved on.
    let (g20, _) = at(&c, 0.020);
    assert!(g20 > g0, "{g20} > {g0}");
    // The extrapolation never passes a full gauge and never goes backwards in time.
    let (far, ready_far) = at(&c, 3_600.0);
    assert_eq!(far, GAUGE_MAX);
    assert!(!ready_far, "only the engine says ready");
    assert_eq!(at(&c, -5.0).0, g0, "a clock that went back shows the mirror as it is");
    // A server message snaps the bar to the mirror: at the instant of a message nothing is
    // extrapolated, so the bar IS the engine's gauge.
    let mut t = 0.5;
    loop {
        t += 0.001;
        if let Some(m) = sim.step(1) {
            c.on_message(t, m);
            break;
        }
        assert!(t < 60.0, "something happens within a minute");
    }
    let mirror_gauge = c.active().unwrap().replay().unwrap().state().actor(ActorId(0)).unwrap().gauge;
    let v = c.view(t).unwrap();
    assert!(v.party[0].gauge == mirror_gauge || v.party[0].ready, "{} vs {mirror_gauge}", v.party[0].gauge);
    let v = c.view(0.0).unwrap();
    assert_eq!((v.party.len(), v.enemies.len()), (1, 1));
    assert!(v.party[0].mine && !v.enemies[0].mine);
    assert_eq!(v.enemies[0].max_hp, 200);
    assert!(!v.waiting_for_answer);
}

#[test]
fn the_title_says_whose_turn_it_is() {
    let mut sim = Sim::new(1, &[ME]);
    let mut c = client(true);
    assert_eq!(c.title_suffix(0.0), "", "no battle, no suffix");
    c.on_message(0.0, sim.start());
    assert!(c.title_suffix(0.0).contains("BATTLE battle#1 vs 2 enemies") && c.title_suffix(0.0).contains("waiting for your gauge"));
    let mut now = 0.0;
    while c.attack_command(now).is_none() {
        now += 0.001;
        if let Some(m) = sim.step(1) {
            c.on_message(now, m);
        }
        assert!(now < 60.0);
    }
    assert!(c.title_suffix(now).contains("YOUR TURN"), "{}", c.title_suffix(now));
}

#[test]
fn messages_that_are_not_battle_messages_change_nothing() {
    let mut c = client(true);
    c.on_message(0.0, ServerMsg::PlayerLeft { entity: EntityId(3) });
    c.on_message(0.0, ServerMsg::Pong { nonce: 1, tick: 2 });
    assert!(!c.in_battle());
    assert_eq!((c.entered(), c.divergences(), c.rejections()), (0, 0, 0));
    assert!(c.drain_notes().is_empty());
    assert!(c.summary().contains("0 battle(s) entered"));
    assert!(format!("{c:?}").contains("BattleClient"));
}

#[test]
fn commands_become_the_messages_the_protocol_defines() {
    let cmd = BattleCmd::Command { battle_id: BattleId(3), actor: ActorId(1), command: Command::Attack { target: ActorId(4) } };
    assert_eq!(
        cmd.to_msg(),
        ClientMsg::BattleCommand { battle_id: BattleId(3), actor: ActorId(1), command: Command::Attack { target: ActorId(4) } }
    );
    assert_eq!(BattleCmd::RequestEncounter { formation: Some(7) }.to_msg(), ClientMsg::RequestEncounter { formation_id: Some(7) });
    assert_eq!(BattleCmd::RequestEncounter { formation: None }.to_msg(), ClientMsg::RequestEncounter { formation_id: None });
}
