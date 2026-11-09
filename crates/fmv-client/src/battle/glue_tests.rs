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

/// The whole chain on the client: a blow of ours and the enemy's counter that wipes the party come in ONE batch. The numbers come up one
/// after the other (the counter's over us after the blow's over the enemy), the banner names the counter when it begins and not before,
/// and the defeat panel is not due until the counter has landed.
#[test]
fn a_counter_in_the_blows_batch_floats_its_number_after_the_blows_and_the_defeat_panel_waits_for_it() {
    use scene::cue::{impact_delay, length_of};
    let server = Server::new();
    // No battle data here: nothing is replayed, the screen follows what the server said.
    let mut c = BattleClient::new(ME);
    c.on_message(1.0, server.start());
    let (foe, me) = (ActorId(1), ActorId(0));
    let ours = Command::Attack { target: foe };
    let counter = ActionRef::EnemyAttack(286);
    let batch = vec![
        BattleEvent::Action { actor: me, action: ActionRef::Attack },
        BattleEvent::Damage { actor: me, target: foe, amount: 40, critical: false },
        BattleEvent::Action { actor: foe, action: counter },
        BattleEvent::Damage { actor: foe, target: me, amount: 999, critical: false },
        BattleEvent::Death { actor: me },
        BattleEvent::Defeat,
    ];
    c.on_message(2.0, ServerMsg::BattleEvent { battle_id: BattleId(1), tick: 10, command: Some(AppliedCommand { actor: me, command: ours }), events: batch });
    c.on_message(2.0, ServerMsg::BattleEnd { battle_id: BattleId(1), outcome: Outcome::Defeat, rewards: vec![] });
    let blow_lands = 2.0 + impact_delay(Some(ActionRef::Attack));
    let counter_begins = 2.0 + length_of(Some(ActionRef::Attack));
    let counter_lands = counter_begins + impact_delay(Some(counter));
    let numbers = |t: f64| -> Vec<(u8, u32)> {
        c.view(t)
            .map(|v| v.popups.iter().filter_map(|p| if let popups::PopupKind::Damage { amount, .. } = p.kind { Some((p.target.0, amount)) } else { None }).collect())
            .unwrap_or_default()
    };
    assert!(numbers(2.0).is_empty(), "nothing has landed when the batch arrives");
    assert_eq!(numbers(blow_lands + 0.01), [(1, 40)], "our blow lands first, and only it");
    assert_eq!(numbers(counter_begins - 0.01), [(1, 40)], "the counter has not begun");
    assert!(counter_lands > blow_lands + 0.5, "the counter lands well after the blow: {counter_lands} against {blow_lands}");
    // Two numbers are up together only when the first is still alive; the counter's is the one over us.
    let at_counter = numbers(counter_lands + 0.01);
    assert!(at_counter.contains(&(0, 999)), "the counter's number over us: {at_counter:?}");
    // The panel of the defeat counts from the moment the counter begins, so it is not due until the counter has landed.
    let finale = |t: f64| c.view(t).and_then(|v| v.finale);
    assert_eq!(finale(counter_begins).map(|f| f.age_ms), Some(0));
    assert!(finale(counter_lands + 0.01).is_some_and(|f| f.age_ms < panels::PANEL_DELAY_MS), "the counter is seen before the panel is due");
    assert!(finale(counter_begins + 2.0).is_some_and(|f| f.age_ms >= panels::PANEL_DELAY_MS && f.dismissible), "and the panel comes");
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

// --- statuses ---------------------------------------------------------------------------------------------------------

use fmv_ff7_battle::status::{Status, StatusSet};

/// A server whose one character starts the battle with `statuses`.
fn sick_server(statuses: &[Status]) -> Server {
    let mut server = Server::new();
    server.setup.party[0].status = statuses.iter().copied().collect::<StatusSet>();
    server.state = SyntheticRules::default().build(&server.setup).expect("a battle");
    server
}

#[test]
fn a_sleeper_holds_a_full_gauge_without_a_menu_and_the_client_says_so() {
    let mut server = sick_server(&[Status::Sleep, Status::Poison]);
    let mut c = started(&server, 1.0);
    // The roster note says what the member starts with, and the view has it on the bar.
    let notes = c.drain_notes();
    assert!(notes.iter().any(|n| n.starts_with("roster actor=0") && n.ends_with("status=Sleep+Poison")), "{notes:?}");
    let v = c.view(1.0).expect("a battle");
    assert_eq!(v.party[0].status, [Status::Sleep, Status::Poison].into_iter().collect::<StatusSet>());
    assert_eq!(v.enemies[0].status, StatusSet::EMPTY);
    // The formation is preemptive: the gauge is full, and the character cannot use the turn.
    assert_eq!(v.party[0].gauge, fmv_ff7_battle::rules::GAUGE_MAX);
    assert!(!v.party[0].ready && v.menu.is_none(), "a sleeper is not waiting for a command");
    assert_eq!(text::status_line(&v).0, "You are asleep");
    assert!(c.title_suffix(1.0).contains("asleep"), "{}", c.title_suffix(1.0));
    assert_eq!(c.menu_key(MenuKey::Confirm, 1.0), MenuReply::Ignored, "no menu, the key is not the menu's");
    assert_eq!(c.attack_command(1.0), None, "nor does the bot try");
    assert_eq!(c.bot_command(1.0), None);
    // The engine's first message announces the statuses, the notes and the popups say so, and the count is per battle.
    c.on_message(1.2, server.until_something_happens());
    let notes = c.drain_notes();
    assert!(notes.contains(&"status: inflicted actor=0 status=Poison".to_string()), "{notes:?}");
    assert!(notes.contains(&"status: inflicted actor=0 status=Sleep".to_string()), "{notes:?}");
    let landed = 1.2 + scene::cue::impact_delay(None);
    let words: Vec<String> = c.view(landed).expect("a battle").popups.iter().map(|p| p.kind.text()).collect();
    assert!(words.contains(&"Sleep".to_string()) && words.contains(&"Poison".to_string()), "{words:?}");
    assert_eq!(c.divergences(), 0);
}

#[test]
fn a_silenced_character_keeps_its_turn_and_loses_its_magic() {
    let server = sick_server(&[Status::Silence]);
    let mut c = started(&server, 1.0);
    // Two Potions in the bag (item 0), so that the Item row has something to open.
    c.on_inventory(1.0, vec![InventoryEntry::new(0, 2)]);
    let v = c.view(1.0).expect("a battle");
    assert!(v.party[0].ready, "silence does not hold a turn");
    let menu = v.menu.expect("a menu");
    let enabled: Vec<bool> = menu.commands.iter().map(|row| row.enabled).collect();
    assert_eq!(enabled, [true, false, true, true, true], "only Magic is greyed out: {:?}", menu.commands);
    assert_eq!(menu.commands[1].label, "Magic");
    // Down to Magic: the hint says why, and Enter does nothing.
    assert_eq!(c.menu_key(MenuKey::Down, 1.0), MenuReply::Handled);
    assert_eq!(c.view(1.0).and_then(|v| v.menu).and_then(|m| m.hint), Some("Silenced: cannot cast"));
    assert_eq!(c.menu_key(MenuKey::Confirm, 1.0), MenuReply::Refused("Silenced: cannot cast"));
    // Item still opens.
    assert_eq!(c.menu_key(MenuKey::Down, 1.0), MenuReply::Handled);
    assert_eq!(c.menu_key(MenuKey::Confirm, 1.0), MenuReply::Handled);
    assert_eq!(c.view(1.0).and_then(|v| v.menu).map(|m| m.level), Some(menu::Level::Items));
}

#[test]
fn the_end_of_a_battle_says_how_many_statuses_it_had() {
    let mut server = sick_server(&[Status::Poison, Status::Regen]);
    let mut c = started(&server, 1.0);
    c.drain_notes();
    // The party fights on (Attack whenever its gauge is full) until the dummy is beaten, and the clock of Poison and
    // Regen runs all the while.
    let mut t = 1.0;
    for _ in 0..4000 {
        if server.state.outcome().is_some() {
            break;
        }
        t += 0.1;
        if server.state.ready_actors().contains(&ActorId(0)) {
            c.on_message(t, server.attack());
        } else {
            c.on_message(t, server.until_something_happens());
        }
    }
    assert!(server.state.outcome().is_some(), "the battle was never decided");
    c.on_message(t + 1.0, server.end());
    let notes = c.drain_notes();
    let counts = notes.iter().find(|n| n.starts_with("status: battle#1 ")).unwrap_or_else(|| panic!("no count: {notes:?}"));
    assert!(counts.contains("inflicted=2") && counts.contains("ended="), "{counts}");
    assert!(counts.split("ticks=").nth(1).and_then(|s| s.trim().parse::<u64>().ok()).is_some(), "a number of turns: {counts}");
    // The count line comes right before the counters' count and the end note.
    let at = notes.iter().position(|n| n == counts).expect("it is there");
    assert_eq!(notes[at + 1], "counter: battle#1 counters=0", "{notes:?}");
    assert!(notes[at + 2].contains("ended in"), "{notes:?}");
    assert_eq!(c.divergences(), 0, "{notes:?}");
}

/// An enemy that acts in the events of a COMMAND is answering it (its counter scripts run inside the command: nothing
/// advances the clock there), and only that is counted and noted; the enemy's own turns come with a step.
#[test]
fn an_enemy_attack_inside_a_commands_events_is_a_counter_and_only_that_is_counted() {
    let server = Server::new();
    // No battle data here: nothing is replayed (and so nothing can diverge), the notes are what the server said.
    let mut c = BattleClient::new(ME);
    c.on_message(1.0, server.start());
    c.drain_notes();
    let foe = ActorId(1);
    let ours = Command::Attack { target: foe };
    let act = |actor: ActorId, action| BattleEvent::Action { actor, action };
    let hit = |actor: ActorId, target: ActorId| BattleEvent::Damage { actor, target, amount: 1, critical: false };
    // A command's batch: our blow, then the enemy's answer to it.
    let batch = vec![act(ActorId(0), ActionRef::Attack), hit(ActorId(0), foe), act(foe, ActionRef::EnemyAttack(286)), hit(foe, ActorId(0))];
    c.on_message(1.1, ServerMsg::BattleEvent { battle_id: BattleId(1), tick: 10, command: Some(AppliedCommand { actor: ActorId(0), command: ours }), events: batch });
    // The enemy's own turn comes with a step, not with a command: it is not a counter.
    c.on_message(1.2, ServerMsg::BattleEvent { battle_id: BattleId(1), tick: 20, command: None, events: vec![act(foe, ActionRef::EnemyAttack(282))] });
    let notes = c.drain_notes();
    let counters: Vec<&String> = notes.iter().filter(|n| n.starts_with("counter: actor=")).collect();
    assert_eq!(counters, ["counter: actor=1 attack=286"], "{notes:?}");
    c.on_message(1.3, ServerMsg::BattleEnd { battle_id: BattleId(1), outcome: Outcome::Defeat, rewards: vec![] });
    let notes = c.drain_notes();
    assert!(notes.contains(&"counter: battle#1 counters=1".to_string()), "{notes:?}");
}

/// What the window does each frame of a battle: the scene is brought up to `t` with what arrived, then given the statuses the
/// panels show (the very tags the view is drawn with: see `scene_for`).
fn frame_of(c: &mut BattleClient, scene: &mut scene::BattleScene, t: f64) -> BattleView {
    scene.update(t, &c.take_events(), c.engine());
    let view = c.view(t).expect("a view");
    if c.engine().is_some() {
        scene.show_statuses(&view);
    }
    view
}

/// The colour factor of actor 0's body in the picture of `scene`.
fn body_colour(scene: &scene::BattleScene) -> [f32; 3] {
    scene.frame(&scene.view(1100, 680), false).tints[0].rgb
}

/// A sleeper wakes when the blow that wakes it LANDS: its tag leaves the panel and its body stands up straight and is itself
/// again at the instant the blow's number floats up, and not when the message that carried the blow arrived (the engine has
/// woken it by then). The same for the status that goes with a blow in every other case: the picture and the number agree.
#[test]
fn a_sleepers_tag_and_body_wake_when_the_blow_that_wakes_it_lands_and_not_when_the_message_arrives() {
    let mut server = sick_server(&[Status::Sleep]);
    let mut c = started(&server, 1.0);
    let start = c.scene_start().expect("a battle to draw");
    let mut kit = scene::ModelKit::new(Box::new(scene::model::StandInModels), None);
    let mut scene = scene::BattleScene::new(&start, c.engine(), &mut kit).expect("a scene");
    let view = frame_of(&mut c, &mut scene, 1.0);
    assert!(view.party[0].status.contains(Status::Sleep), "asleep from the start");
    assert_ne!(body_colour(&scene), [1.0; 3], "and the body says so");
    // Time passes (the sleeper's own gauge is full, the dummy's fills) until the dummy's blow wakes the sleeper.
    let mut t = 1.0;
    let mut woken = None;
    for _ in 0..4000 {
        t += 0.05;
        let msg = server.until_something_happens();
        let wakes = |m: &ServerMsg| matches!(m, ServerMsg::BattleEvent { events, .. } if events.iter().any(|e| matches!(e, BattleEvent::StatusEnded { status: Status::Sleep, .. })));
        let found = wakes(&msg);
        let ServerMsg::BattleEvent { events, .. } = &msg else { unreachable!("a battle message") };
        let landing = scene::cue::cues(events).iter().find_map(|cue| cue.events.iter().find(|e| matches!(e, BattleEvent::StatusEnded { .. })).map(|e| cue.delay_of(e)));
        c.on_message(t, msg);
        frame_of(&mut c, &mut scene, t);
        if found {
            woken = landing.map(|after| (t, after));
            break;
        }
    }
    let (arrival, after) = woken.expect("a blow woke the sleeper");
    assert!(after > 0.0 && after <= scene::cue::MAX_IMPACT_DELAY, "it lands within the cap: {after}");
    // The engine has woken it the moment the message arrived...
    assert!(!c.engine().expect("a battle").statuses(ActorId(0)).contains(Status::Sleep));
    // ...the panel and the body do not know it yet...
    let at = |c: &BattleClient, t: f64| c.view(t).expect("a view").party[0].status.contains(Status::Sleep);
    assert!(at(&c, arrival), "the tag is still on the panel when the message arrives");
    assert_ne!(body_colour(&scene), [1.0; 3], "and the body is still asleep");
    let view = frame_of(&mut c, &mut scene, arrival + after - 0.01);
    assert!(view.party[0].status.contains(Status::Sleep), "a moment before the blow lands");
    assert_ne!(body_colour(&scene), [1.0; 3]);
    // ...and both change at the instant the blow lands, the instant its number is born.
    let view = frame_of(&mut c, &mut scene, arrival + after + 0.01);
    assert!(!view.party[0].status.contains(Status::Sleep), "the tag is gone as the blow lands");
    assert_eq!(body_colour(&scene), [1.0; 3], "and so is the sleeper's dim blue");
    assert!(view.popups.iter().any(|p| p.target == ActorId(0) && matches!(p.kind, popups::PopupKind::Damage { .. })), "with its number: {:?}", view.popups);
    assert_eq!(c.divergences(), 0);
}
