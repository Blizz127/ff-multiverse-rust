//! The battle UI as the client wires it: a real mirror (`BattleClient`) fed by a simulated server
//! (`SyntheticRules` plays the game's rules; no game files), looked at through `view`, `menu_key`,
//! `lingering` and the window title. The pieces themselves are tested in their own modules; this is
//! about how the client puts them together.

use super::*;
use fmv_core::EntityId;
use fmv_ff7_battle::rules::BattleState;
use fmv_protocol::{split_rewards, BattleMember, SyntheticRules};

const ME: AccountId = AccountId(11);
const FOE: u16 = 3;

/// A server for one battle of ours against one dummy (the formation is preemptive: our gauge is full
/// at once).
struct Server {
    id: BattleId,
    setup: BattleSetup,
    state: BattleState,
}

impl Server {
    fn new() -> Self {
        let setup = BattleSetup {
            formation_id: FOE,
            seed: 5,
            party: vec![BattleMember::player(EntityId(100), ME, "P0", 0, 6, 0)],
            preemptive: true,
            back_attack: false,
            battle_speed: 0,
            ticks_per_second: 1000,
        };
        let state = SyntheticRules::default().build(&setup).expect("a battle");
        Server { id: BattleId(1), setup, state }
    }

    /// The same battle under another id.
    fn with_id(mut self, id: u64) -> Self {
        self.id = BattleId(id);
        self
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

    /// Our character attacks the dummy.
    fn attack(&mut self) -> ServerMsg {
        let target = self.state.enemy_ids()[0];
        let command = Command::Attack { target };
        let events = self.state.submit_command(ActorId(0), command).expect("our gauge is full");
        ServerMsg::BattleEvent { battle_id: self.id, tick: self.state.tick(), events, command: Some(AppliedCommand { actor: ActorId(0), command }) }
    }

    /// Let time pass until something happens (the dummy attacking us, our gauge filling).
    fn until_something_happens(&mut self) -> ServerMsg {
        loop {
            let events = self.state.step_ticks(5);
            if !events.is_empty() {
                return ServerMsg::BattleEvent { battle_id: self.id, tick: self.state.tick(), events, command: None };
            }
        }
    }

    fn end(&self) -> ServerMsg {
        let outcome = self.state.outcome().expect("over");
        let rewards = if outcome == Outcome::Victory { split_rewards(self.state.rewards(), &[(ME, self.state.actor(ActorId(0)).is_some_and(Combatant::is_alive))]) } else { vec![] };
        ServerMsg::BattleEnd { battle_id: self.id, outcome, rewards }
    }
}

fn client() -> BattleClient {
    let mut c = BattleClient::new(ME);
    c.set_rules(Some(Arc::new(SyntheticRules::default())));
    c
}

/// A client that has seen the start of `server`'s battle at `t`.
fn started(server: &Server, t: f64) -> BattleClient {
    let mut c = client();
    c.on_message(t, server.start());
    c
}

#[test]
fn the_view_names_fighters_by_actor_and_carries_their_mp() {
    let server = Server::new();
    let c = started(&server, 1.0);
    let v = c.view(1.0).expect("a battle");
    assert_eq!(v.party.iter().map(|b| b.actor).collect::<Vec<_>>(), [ActorId(0)]);
    assert_eq!(v.enemies.iter().map(|b| b.actor).collect::<Vec<_>>(), [ActorId(1)], "the enemies' ids follow the party's");
    assert!(v.party[0].mine && !v.enemies[0].mine);
    // A party member is its player's own name (the roster's), not the canon character's whose combat template it
    // uses (slot 0 is Cloud's); an enemy keeps the name of the engine.
    let engine = server.state.actor(ActorId(0)).expect("us");
    assert_eq!(engine.name, "Cloud", "the engine knows the slot's character");
    assert_eq!(v.party[0].name, "P0");
    assert_eq!(v.enemies[0].name, server.state.actor(ActorId(1)).expect("the dummy").name);
    assert_eq!((v.party[0].mp, v.party[0].max_mp), (engine.mp, engine.max_mp));
    assert!(v.menu.is_some() && v.popups.is_empty() && v.banner.is_none() && v.finale.is_none());
}

#[test]
fn a_ready_character_gets_a_menu_and_it_closes_while_a_command_waits_and_after_it() {
    let mut server = Server::new();
    let mut c = started(&server, 1.0);
    let level = |c: &BattleClient, t: f64| c.view(t).and_then(|v| v.menu).map(|m| m.level);
    assert_eq!(level(&c, 1.0), Some(menu::Level::Commands), "our gauge is full from the start");
    // Attack, then the target: a second Confirm sends the attack on the dummy.
    assert_eq!(c.menu_key(MenuKey::Confirm, 1.0), MenuReply::Handled);
    assert_eq!(level(&c, 1.0), Some(menu::Level::Target));
    let sent = c.menu_key(MenuKey::Confirm, 1.1);
    let expected = BattleCmd::Command { battle_id: BattleId(1), actor: ActorId(0), command: Command::Attack { target: ActorId(1) } };
    assert_eq!(sent, MenuReply::Send(expected));
    // Sent: the menu is closed until the server answers, and a second command is refused meanwhile.
    c.note_sent(&expected, 1.1);
    assert_eq!(level(&c, 1.2), None, "waiting for the server");
    assert_eq!(c.menu_key(MenuKey::Confirm, 1.2), MenuReply::Ignored);
    // The server's answer applies it: our gauge is empty, the menu stays closed.
    c.on_message(1.3, server.attack());
    assert_eq!(level(&c, 1.3), None);
    assert_eq!(c.menu_key(MenuKey::Down, 1.3), MenuReply::Ignored, "no menu, the key is not the menu's");
    assert!(!c.back(1.3), "and Esc is the window's");
    // A command the server refused: the menu comes back at the top.
    let mut c = started(&Server::new(), 1.0);
    c.menu_key(MenuKey::Confirm, 1.0);
    let MenuReply::Send(cmd) = c.menu_key(MenuKey::Confirm, 1.0) else { panic!("a command") };
    c.note_sent(&cmd, 1.0);
    c.on_message(1.1, ServerMsg::BattleRejected { battle_id: Some(BattleId(1)), reason: BattleRejection::GaugeNotFull });
    assert_eq!(level(&c, 1.1), Some(menu::Level::Commands), "the refusal reopens the menu at the top level");
}

#[test]
fn hits_float_numbers_over_their_target_and_they_go_with_time() {
    let mut server = Server::new();
    let mut c = started(&server, 1.0);
    c.on_message(1.5, server.attack());
    // The number is born when the blow lands, not when the message arrives.
    let landed = 1.5 + scene::cue::impact_delay(Some(fmv_protocol::ActionRef::Attack));
    assert!(c.view(1.5).expect("a battle").popups.is_empty(), "the blow has not landed yet");
    let v = c.view(landed).expect("a battle");
    let hit = v.popups.iter().find(|p| p.target == ActorId(1)).expect("a number over the dummy");
    assert!(matches!(hit.kind, popups::PopupKind::Damage { .. } | popups::PopupKind::Miss), "{hit:?}");
    assert_eq!(hit.age_ms, 0);
    assert_eq!(c.view(landed + 0.3).expect("a battle").popups.first().map(|p| p.age_ms), Some(300), "it ages with the clock");
    assert!(c.view(landed + popups::LIFETIME_S + 0.1).expect("a battle").popups.is_empty(), "and is gone after its lifetime");
    // We stand still: the dummy's attack floats a number over us (our own gauge filling comes first
    // or after, a message without a hit makes no number).
    let over_us = |c: &BattleClient, t: f64| c.view(t).expect("a battle").popups.iter().any(|p| p.target == ActorId(0));
    let mut t = 4.0;
    loop {
        assert!(t < 400.0, "the dummy never attacked");
        t += 0.01;
        c.on_message(t, server.until_something_happens());
        // The number is born when the blow lands, a moment after the message: look then, with no
        // message in between (the battle goes on without us and a wait could outlast it).
        if over_us(&c, t + scene::cue::MAX_IMPACT_DELAY) {
            break;
        }
    }
    assert!(server.state.actor(ActorId(0)).is_some_and(|me| me.hp <= me.max_hp), "the number is for something that happened");
}

/// Play the battle to its end: attack whenever the gauge is full.
fn win(server: &mut Server, c: &mut BattleClient, mut t: f64) -> f64 {
    for _ in 0..10_000 {
        if server.state.outcome().is_some() {
            break;
        }
        t += 0.01;
        let msg = if server.state.ready_actors().contains(&ActorId(0)) { server.attack() } else { server.until_something_happens() };
        c.on_message(t, msg);
    }
    assert_eq!(server.state.outcome(), Some(Outcome::Victory), "the party wins");
    t
}

#[test]
fn the_end_lingers_with_the_last_blow_the_rewards_and_a_panel_until_a_key_or_time_closes_it() {
    let mut server = Server::new();
    let mut c = started(&server, 1.0);
    let t = win(&mut server, &mut c, 1.0);
    c.on_message(t, server.end());
    assert!(!c.in_battle(), "the server freed the player");
    assert_eq!(c.victories(), 1);
    assert!(c.lingering(t + 0.5));
    let v = c.view(t + 0.5).expect("the view lingers");
    // The killing blow lands half a second after it arrived; the beaten dummy stays in the list until
    // its collapse begins (a beat later), and is dead in the last view after that.
    assert!(v.enemies.iter().all(|b| b.alive), "still listed while the blow lands");
    assert!(c.view(t + scene::cue::MAX_DEATH_DELAY + 0.01).expect("the view lingers").enemies.iter().all(|b| !b.alive), "the last view: the dummy is dead");
    assert!(v.menu.is_none() && v.banner.is_none());
    assert!(v.popups.iter().any(|p| p.target == ActorId(1)), "the last blow's number floats up when it lands");
    let finale = v.finale.expect("the panel's data");
    assert_eq!(finale.outcome, Outcome::Victory);
    let reward = finale.reward.expect("we are in the party, so we are paid");
    assert_eq!((reward.exp, reward.gil, reward.ap), (server.state.rewards().exp, server.state.rewards().gil, server.state.rewards().ap));
    assert!(!finale.dismissible, "too soon to close it");
    // The window title says nothing about a battle that is over.
    assert_eq!(c.title_suffix(t + 0.5), "");
    // A key before its time is taken and does nothing; after it, it closes the panel.
    assert_eq!(c.menu_key(MenuKey::Confirm, t + 0.5), MenuReply::Handled);
    assert!(c.lingering(t + 0.6));
    assert!(c.view(t + 2.0).and_then(|v| v.finale).is_some_and(|f| f.dismissible));
    assert_eq!(c.menu_key(MenuKey::Confirm, t + 2.0), MenuReply::Handled);
    assert!(!c.lingering(t + 2.1) && c.view(t + 2.1).is_none());
    // Left alone, the panel goes by itself.
    let mut server = Server::new();
    let mut c = started(&server, 1.0);
    let t = win(&mut server, &mut c, 1.0);
    c.on_message(t, server.end());
    assert!(c.lingering(t + hud::LINGER_S - 0.1) && !c.lingering(t + hud::LINGER_S + 0.1));
}

#[test]
fn a_lost_battle_lingers_with_a_defeat_and_no_reward() {
    let server = Server::new();
    let mut c = started(&server, 1.0);
    c.on_message(2.0, ServerMsg::BattleEnd { battle_id: BattleId(1), outcome: Outcome::Defeat, rewards: vec![] });
    let f = c.view(2.5).and_then(|v| v.finale).expect("a defeat panel");
    assert_eq!((f.outcome, f.reward), (Outcome::Defeat, None));
}

#[test]
fn a_new_battle_wipes_the_lingering_one_and_its_numbers() {
    let mut server = Server::new();
    let mut c = started(&server, 1.0);
    let t = win(&mut server, &mut c, 1.0);
    c.on_message(t, server.end());
    assert!(c.lingering(t + 0.1));
    // The next battle starts while the last one's panel is still up: the panel and its numbers are gone.
    c.on_message(t + 0.2, Server::new().with_id(2).start());
    assert!(c.in_battle() && !c.lingering(t + 0.3));
    let v = c.view(t + 0.3).expect("the new battle");
    assert_eq!(v.id, BattleId(2));
    assert!(v.finale.is_none() && v.popups.is_empty(), "nothing of the last battle is left on screen");
    assert!(v.menu.is_some(), "and our gauge is full in this one too");
}
