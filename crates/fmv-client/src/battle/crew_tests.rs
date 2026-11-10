//! Commanding the canon companions (protocol v17): when the roster names this client the commander of Cloud and Barret
//! (`BattleMember::commander`) it commands them as well as its own character, the menu belongs to the member whose turn came
//! first and moves on as FF7's does, `Tab` swaps it, and the bot commands every member it is given. Against a simulated server
//! (`fmv_protocol::SyntheticRules`; no game files).

use super::*;
use crate::party::PartyMirror;
use fmv_core::EntityId;
use fmv_ff7_battle::rules::BattleState;
use fmv_protocol::{split_rewards, SyntheticRules};

const ME: AccountId = AccountId(11);
const PEER: AccountId = AccountId(12);

/// Cloud and Barret, commanded by `commander` (the server when `None`), then one player per account.
fn roster(commander: Option<AccountId>, accounts: &[AccountId]) -> Vec<BattleMember> {
    let companion = |name: &str, character: u8| {
        let member = BattleMember::companion(name, character, 6, 0);
        match commander {
            Some(account) => member.commanded_by(account),
            None => member,
        }
    };
    let players = accounts.iter().enumerate().map(|(i, &account)| BattleMember::player(EntityId(100 + i as u64), account, format!("P{i}"), i as u8, 6, 0));
    [companion("Cloud", 0), companion("Barret", 1)].into_iter().chain(players).collect()
}

/// A server for one battle, as the real one drives it: the engine built from the roster, the messages it announces.
struct Server {
    setup: BattleSetup,
    state: BattleState,
}

const ENEMY_HP: u32 = 100_000;

impl Server {
    /// A preemptive battle (every gauge is full at the first step) against enemies of `enemy_hp`.
    fn new(party: Vec<BattleMember>, enemy_hp: u32) -> Self {
        let setup = BattleSetup { victory_fanfare: true, game_over_on_defeat: false, initial_party: None, escape_policy: fmv_protocol::EscapePolicy::Formation, formation_id: 1, seed: 99, party, preemptive: true, back_attack: false, battle_speed: 0, ticks_per_second: 1000 };
        let state = SyntheticRules { enemy_hp, ..SyntheticRules::default() }.build(&setup).unwrap();
        Server { setup, state }
    }

    fn start(&self) -> ServerMsg {
        ServerMsg::EncounterStart {
            victory_fanfare: true,
            game_over_on_defeat: false,
            initial_party: None,
            escape_policy: fmv_protocol::EscapePolicy::Formation,
            battle_id: BattleId(1),
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

    /// Advance the engine by `ticks`; the message only if something happened (as the server does).
    fn step(&mut self, ticks: u64) -> Option<ServerMsg> {
        let events = self.state.step_ticks(ticks);
        (!events.is_empty()).then(|| ServerMsg::BattleEvent { battle_id: BattleId(1), tick: self.state.tick(), events, command: None })
    }

    /// Apply a member's command and announce it with the command attached.
    fn command(&mut self, actor: ActorId, command: Command) -> ServerMsg {
        let events = self.state.submit_command(actor, command).expect("the engine takes it");
        ServerMsg::BattleEvent { battle_id: BattleId(1), tick: self.state.tick(), events, command: Some(AppliedCommand { actor, command }) }
    }

    fn attack(&mut self, actor: ActorId) -> ServerMsg {
        let target = self.state.enemy_ids().into_iter().find(|e| self.state.actor(*e).is_some_and(|c| c.is_alive())).expect("an enemy stands");
        self.command(actor, Command::Attack { target })
    }

    fn end(&self) -> ServerMsg {
        let outcome = self.state.outcome().expect("over");
        let roster = fmv_protocol::reward_roster(&self.setup.party, &self.state);
        let rewards = if outcome == Outcome::Victory { split_rewards(self.state.rewards(), &roster) } else { vec![] };
        ServerMsg::BattleEnd { battle_id: BattleId(1), outcome, rewards }
    }
}

/// A client of `account` in a battle that has begun and run its first step: every gauge is full. The enemies survive the whole
/// test.
fn begun(party: Vec<BattleMember>, account: AccountId) -> (BattleClient, Server) {
    begun_against(party, account, ENEMY_HP)
}

/// [`begun`] against enemies of `enemy_hp` (the client's own rules say the same, or its replay would not match).
fn begun_against(party: Vec<BattleMember>, account: AccountId, enemy_hp: u32) -> (BattleClient, Server) {
    let mut server = Server::new(party, enemy_hp);
    let mut c = BattleClient::new(account);
    c.set_rules(Some(Arc::new(SyntheticRules { enemy_hp, ..SyntheticRules::default() })));
    c.on_message(0.0, server.start());
    c.on_message(0.01, server.step(10).expect("the gauges fill"));
    (c, server)
}

fn turns(c: &BattleClient) -> Vec<u8> {
    c.active().unwrap().turns().iter().map(|a| a.0).collect()
}

fn commanded(c: &BattleClient) -> Vec<u8> {
    c.active().unwrap().commanded().iter().map(|a| a.0).collect()
}

/// The actor of the party member the view marks as the one the menu is for.
fn marked(c: &BattleClient, now: f64) -> Vec<u8> {
    c.view(now).unwrap().party.iter().filter(|b| b.mine).map(|b| b.actor.0).collect()
}

/// Send the command a sequence of menu keys makes, if it makes one.
fn choose(c: &mut BattleClient, keys: &[MenuKey], now: f64) -> Option<BattleCmd> {
    let mut sent = None;
    for key in keys {
        match c.menu_key(*key, now) {
            MenuReply::Send(cmd) => sent = Some(cmd),
            MenuReply::Handled | MenuReply::Ignored => {}
            MenuReply::Refused(why) => panic!("the menu refused {key:?}: {why}"),
        }
    }
    sent
}

fn attack_of(cmd: Option<BattleCmd>) -> (u8, bool) {
    match cmd {
        Some(BattleCmd::Command { actor, command: Command::Attack { .. }, .. }) => (actor.0, true),
        Some(BattleCmd::Command { actor, .. }) => (actor.0, false),
        other => panic!("no command: {other:?}"),
    }
}

#[test]
fn a_roster_that_names_us_makes_the_canon_pair_ours_to_command_and_one_that_does_not_leaves_them_to_the_server() {
    // Named: the pair and ourselves, in actor order.
    let (mut c, _) = begun(roster(Some(ME), &[ME]), ME);
    assert_eq!(commanded(&c), [0, 1, 2]);
    assert_eq!(c.active().unwrap().my_actor(), Some(ActorId(2)));
    let n = c.drain_notes().join("\n");
    assert!(n.contains("you command the canon companions as well as your own character (actors [0, 1])"), "{n}");
    // The server's (no commander, as every v16 roster): only ourselves, and no word about the companions.
    let (mut c, _) = begun(roster(None, &[ME]), ME);
    assert_eq!(commanded(&c), [2]);
    assert!(!c.drain_notes().join("\n").contains("you command the canon companions"));
    // Another player's: not ours, whoever is first in the roster (the commander is the leader, which the roster says).
    let (mut c, _) = begun(roster(Some(PEER), &[ME, PEER]), ME);
    assert_eq!(commanded(&c), [2]);
    assert!(!c.drain_notes().join("\n").contains("you command the canon companions"));
    let (c, _) = begun(roster(Some(PEER), &[ME, PEER]), PEER);
    assert_eq!(commanded(&c), [0, 1, 3], "the other player of the same battle commands the pair and itself");
    // A client that is not in the battle at all commands nobody.
    let (c, _) = begun(roster(Some(PEER), &[PEER]), ME);
    assert_eq!((commanded(&c), c.active().unwrap().my_actor()), (vec![], None));
}

#[test]
fn the_turns_are_queued_in_the_order_the_gauges_filled_and_the_first_has_the_menu() {
    let (c, _) = begun(roster(Some(ME), &[ME]), ME);
    // A preemptive start fills all three at once: in actor order, the pair first.
    assert_eq!(turns(&c), [0, 1, 2]);
    assert_eq!(marked(&c, 0.02), [0], "the panels mark Cloud: his is the menu on offer");
    let view = c.view(0.02).unwrap();
    assert!(view.menu.is_some(), "Cloud's menu is open");
    assert!(view.party.iter().all(|b| b.ready), "everybody of ours waits");
    assert!(c.title_suffix(0.02).contains("CLOUD'S TURN"), "{}", c.title_suffix(0.02));
}

#[test]
fn the_menu_moves_on_to_the_next_ready_member_when_a_command_is_applied() {
    let (mut c, mut server) = begun(roster(Some(ME), &[ME]), ME);
    let mut now = 0.02;
    let mut given = Vec::new();
    for expected in [0u8, 1, 2] {
        assert_eq!(marked(&c, now), [expected], "the menu is for actor {expected}");
        // Confirm on Attack, then on the first target: the command of the member whose menu it is.
        let cmd = choose(&mut c, &[MenuKey::Confirm, MenuKey::Confirm], now).expect("a command");
        assert_eq!(attack_of(Some(cmd)), (expected, true));
        c.note_sent(&cmd, now);
        assert!(c.view(now).unwrap().waiting_for_answer, "the command is in flight");
        now += 0.05;
        let BattleCmd::Command { actor, .. } = cmd else { unreachable!() };
        c.on_message(now, server.attack(actor));
        given.push(actor.0);
        assert!(!c.view(now).unwrap().waiting_for_answer);
    }
    assert_eq!(given, [0, 1, 2]);
    // Everybody has acted, so nobody's turn is queued and the panels go back to marking our own character.
    assert_eq!(turns(&c), Vec::<u8>::new());
    assert_eq!(marked(&c, now), [2]);
    assert!(c.view(now).unwrap().menu.is_none());
}

#[test]
fn another_members_gauge_filling_never_takes_the_menu_from_a_choice_being_made() {
    let (mut c, mut server) = begun(roster(Some(ME), &[ME]), ME);
    let mut now = 0.02;
    // Cloud acts; Barret and we are still waiting, and Barret has the menu.
    let cmd = choose(&mut c, &[MenuKey::Confirm, MenuKey::Confirm], now).expect("Cloud's attack");
    c.note_sent(&cmd, now);
    now += 0.05;
    c.on_message(now, server.attack(ActorId(0)));
    assert_eq!(turns(&c), [1, 2]);
    // The player opens Barret's target selection...
    assert_eq!(c.menu_key(MenuKey::Confirm, now), MenuReply::Handled);
    assert!(!c.hud.menu_at_commands(), "choosing a target");
    // ... and Cloud's gauge fills again meanwhile (a message that announces nothing but that).
    let mut filled = false;
    for _ in 0..2_000 {
        now += 0.01;
        if let Some(msg) = server.step(10) {
            c.on_message(now, msg);
        }
        if turns(&c).contains(&0) {
            filled = true;
            break;
        }
    }
    assert!(filled, "Cloud's gauge fills");
    assert_eq!(turns(&c), [1, 2, 0], "Cloud joins at the back");
    assert_eq!(marked(&c, now), [1], "the menu stays with Barret");
    assert!(!c.hud.menu_at_commands(), "and is still at the target selection, not yanked back to the commands");
    // The choice is Barret's.
    let cmd = choose(&mut c, &[MenuKey::Confirm], now).expect("Barret's attack");
    assert_eq!(attack_of(Some(cmd)), (1, true));
}

#[test]
fn tab_swaps_the_menu_to_the_next_ready_member_only_at_the_command_list() {
    let (mut c, _) = begun(roster(Some(ME), &[ME]), ME);
    let now = 0.02;
    assert_eq!(marked(&c, now), [0]);
    // At the command list: Cloud goes to the back and Barret has the menu.
    assert_eq!(c.swap_turn(), MenuReply::Handled);
    assert_eq!((turns(&c), marked(&c, now)), (vec![1, 2, 0], vec![1]));
    assert_eq!(c.swap_turn(), MenuReply::Handled);
    assert_eq!((turns(&c), marked(&c, now)), (vec![2, 0, 1], vec![2]), "and on to us");
    assert_eq!(c.swap_turn(), MenuReply::Handled);
    assert_eq!(marked(&c, now), [0], "round to Cloud again");
    // The menu is Barret's after a swap: the command it sends is his.
    assert_eq!(c.swap_turn(), MenuReply::Handled);
    assert_eq!(c.menu_key(MenuKey::Confirm, now), MenuReply::Handled, "opens the target selection");
    // In the middle of a choice a swap does nothing at all.
    assert_eq!(c.swap_turn(), MenuReply::Ignored);
    assert_eq!(marked(&c, now), [1]);
    let cmd = choose(&mut c, &[MenuKey::Confirm], now).expect("Barret's attack");
    assert_eq!(attack_of(Some(cmd)), (1, true));
}

#[test]
fn a_swap_has_nothing_to_swap_with_one_member_ready_or_for_a_client_that_commands_only_itself() {
    // Only ourselves: nobody to swap with, so nothing happens.
    let (mut c, _) = begun(roster(None, &[ME]), ME);
    assert_eq!(turns(&c), [2]);
    assert_eq!(c.swap_turn(), MenuReply::Ignored);
    assert_eq!(marked(&c, 0.02), [2]);
    // Three of ours are ready but one acts: two are left, and then one.
    let (mut c, mut server) = begun(roster(Some(ME), &[ME]), ME);
    for actor in [0u8, 1] {
        c.on_message(0.1, server.attack(ActorId(actor)));
    }
    assert_eq!(turns(&c), [2]);
    assert_eq!(c.swap_turn(), MenuReply::Ignored, "we are the only one waiting");
    // Out of battle there is nothing at all.
    let mut idle = BattleClient::new(ME);
    assert_eq!(idle.swap_turn(), MenuReply::Ignored);
}

#[test]
fn the_menu_offers_the_spells_and_limit_of_the_member_it_is_for() {
    let (mut c, _) = begun(roster(Some(ME), &[ME]), ME);
    // The synthetic rules give the canon pair a limit break and the players none: the Limit row is Cloud's and Barret's.
    assert_eq!(marked(&c, 0.02), [0]);
    assert!(c.menu_model_mut().limit.is_some(), "Cloud has a limit break: his is the menu on offer");
    c.swap_turn();
    c.swap_turn();
    assert_eq!(marked(&c, 0.02), [2]);
    assert!(c.menu_model_mut().limit.is_none(), "a player has no limit break: the menu is the player's, not Cloud's");
}

#[test]
fn the_bot_commands_every_member_it_is_given_the_one_that_waited_longest_first_and_one_at_a_time() {
    let (mut c, mut server) = begun(roster(Some(ME), &[ME]), ME);
    let mut now = 0.02;
    let mut order = Vec::new();
    for _ in 0..3 {
        let cmd = c.bot_command(now).expect("somebody of ours is ready");
        let BattleCmd::Command { actor, command, .. } = cmd else { panic!("{cmd:?}") };
        assert!(matches!(command, Command::Attack { .. }), "a healthy member attacks");
        order.push(actor.0);
        c.note_sent(&cmd, now);
        // One at a time: the next waits for the server's answer to this one.
        assert_eq!(c.bot_command(now), None);
        now += 0.05;
        c.on_message(now, server.attack(actor));
    }
    assert_eq!(order, [0, 1, 2], "in the order the gauges filled");
    assert_eq!(c.bot_command(now), None, "nobody of ours is ready");
    assert_eq!((c.my_commands_applied(), c.active().unwrap().my_actor()), (1, Some(ActorId(2))), "our own commands are counted apart");
}

#[test]
fn a_bot_that_is_not_the_commander_commands_only_itself() {
    let (c, _) = begun(roster(Some(PEER), &[PEER, ME]), ME);
    assert_eq!(commanded(&c), [3]);
    let cmd = c.bot_command(0.02).expect("our own turn");
    assert_eq!(attack_of(Some(cmd)), (3, true));
    assert_eq!(c.crew_command(0.02), None, "it has no companion to command");
    // With the server commanding the pair (no commander at all) the same.
    let (c, _) = begun(roster(None, &[ME]), ME);
    assert_eq!(attack_of(c.bot_command(0.02)), (2, true));
    assert_eq!(c.crew_command(0.02), None);
}

#[test]
fn a_script_gives_our_own_characters_commands_and_never_keeps_the_companions_waiting() {
    let (mut c, mut server) = begun(roster(Some(ME), &[ME]), ME);
    let mut bot = BattleBot::new(true, false, 1).with_script(vec![ScriptStep::Defend, ScriptStep::Defend]);
    let party = PartyMirror::new(ME);
    let mut now = 0.02;
    // The script comes first for our own character, which is ready: its Defend, not Cloud's attack.
    let sent = bot.step(now, &c, &party);
    let [cmd @ BattleCmd::Command { actor, command: Command::Defend, .. }] = sent[..] else { panic!("{sent:?}") };
    assert_eq!(actor, ActorId(2));
    c.note_sent(&cmd, now);
    now += 0.05;
    c.on_message(now, server.command(actor, Command::Defend));
    // Our own gauge is empty now and the script has an entry left: it waits for our turn, and the companions play meanwhile.
    let sent = bot.step(now, &c, &party);
    let [cmd @ BattleCmd::Command { actor, command: Command::Attack { .. }, .. }] = sent[..] else { panic!("{sent:?}") };
    assert_eq!(actor, ActorId(0), "Cloud's, though the script is not used up");
    c.note_sent(&cmd, now);
    now += 0.05;
    c.on_message(now, server.attack(actor));
    let sent = bot.step(now, &c, &party);
    let [BattleCmd::Command { actor, command: Command::Attack { .. }, .. }] = sent[..] else { panic!("{sent:?}") };
    assert_eq!(actor, ActorId(1), "then Barret's");
    // The script's entry counts only commands of our own character: Cloud's and Barret's never advanced it.
    assert_eq!(c.my_commands_applied(), 1);
}

#[test]
fn what_the_companions_were_commanded_is_counted_apart_from_our_own_commands() {
    // Enemies that die in a few rounds, so that the battle ends.
    let (mut c, mut server) = begun_against(roster(Some(ME), &[ME]), ME, 200);
    let mut now = 0.02;
    for actor in [0u8, 1, 2] {
        now += 0.05;
        c.on_message(now, server.attack(ActorId(actor)));
    }
    assert_eq!(c.my_commands_applied(), 1, "only the command of our own character");
    // Kill the enemies to end the battle (the server's end is a message of its own), then read what the client said.
    let mut server_msgs = 0;
    while server.state.outcome().is_none() {
        server_msgs += 1;
        assert!(server_msgs < 100_000, "the battle never ended");
        now += 0.05;
        if let Some(msg) = server.step(50) {
            c.on_message(now, msg);
        }
        for actor in server.state.ready_actors() {
            if server.state.outcome().is_none() {
                c.on_message(now, server.attack(actor));
            }
        }
    }
    c.on_message(now, server.end());
    let summary = c.summary();
    assert!(summary.contains("command(s) of ours applied (") && summary.ends_with(" command(s) for the canon companions applied"), "{summary}");
    let n = c.drain_notes().join("\n");
    assert!(n.contains("crew: battle#1 commands="), "{n}");
    assert_eq!(c.divergences(), 0, "the replay agrees with the server whoever commanded the companions: {n}");
    assert_eq!(c.verified(), 1);
    // Ours alone are the ones the summary and the script call ours.
    let ours: u64 = summary.split(" command(s) of ours applied").next().and_then(|head| head.rsplit(", ").next()).and_then(|n| n.parse().ok()).expect("a number");
    assert_eq!(ours, c.commands_applied());
    assert!(ours >= 1);
}
