//! Spells, items, Defend, limit breaks, escape and the inventory through a real mirror: a simulated server
//! runs the engine (`SyntheticRules`, or hand-made rules for what those cannot set up), the client mirrors
//! it with the same rules, and the tests press the real menu keys, send what comes out, let the server
//! apply it and look at what the client shows and counts. No game files are involved.

use super::*;
use fmv_core::EntityId;
use fmv_ff7_battle::rules::{element, ActionSpec, Aim, EnemyAction, EscapeRule, Formula, ItemData, ItemTable, LimitBreak, Reward, Spell, Target};
use fmv_protocol::{split_rewards, BattleBuildError, BattleMember, FormationInfo, SyntheticRules};
use super::menu::{AimAt, Reach};
use super::popups::PopupKind;

const ME: AccountId = AccountId(11);
const PEER: AccountId = AccountId(12);

/// The rows of the command menu.
const MAGIC: usize = 1;
const ITEM: usize = 2;
const DEFEND: usize = 3;

const MAX_HP: u32 = 200;
const MAX_MP: u32 = 60;

/// Hand-made rules for the situations `SyntheticRules` cannot set up: our character starts hurt or short
/// of MP, the second member starts dead, the spells are listed so that the cheapest healer is not the
/// first, one item damages, and the enemy can have attacks of its own. The client mirrors with the very
/// same rules the simulated server builds its engine from, as in the real thing.
#[derive(Clone)]
struct Scenario {
    /// Our character's HP and MP at the start (the most are 200 and 60).
    hp: u32,
    mp: u32,
    /// What every party member knows.
    spells: Vec<Spell>,
    /// The second member starts dead.
    ally_dead: bool,
    enemy_hp: u32,
    /// The enemy's own attacks (none: it attacks plainly).
    enemy_attacks: Vec<EnemyAction>,
    /// Every member's limit break (none: no limit, so no gauge), whose starting gauge is the member's own
    /// `BattleMember::limit`, as in the real builder.
    limit: Option<LimitBreak>,
    /// How hard the battle is to run from (none: it cannot be).
    escape: Option<EscapeRule>,
    /// How many whole-side casts of the Cure and Fire kind every member's All materia allows (protocol v8).
    all: u8,
}

impl Scenario {
    fn new() -> Self {
        Self {
            hp: MAX_HP,
            mp: MAX_MP,
            spells: Self::spells(),
            ally_dead: false,
            enemy_hp: 1_200,
            enemy_attacks: Vec::new(),
            limit: None,
            escape: Some(EscapeRule { difficulty: 1, sure: true }),
            all: 0,
        }
    }

    /// An enemy blow of exactly 20 damage that never misses (a flat formula): `n` of them fill a gauge by a
    /// known amount (`20 * 76800 / (200 * 140)` = 54 for the Braver below).
    fn flat_blow() -> EnemyAction {
        EnemyAction { id: 1, spec: ActionSpec { formula: Formula::Flat, ..ActionSpec::damage_item(1, 0) } }
    }

    /// Braver: 3x, level 1 divisor 140 (the Cloud of the owner's file).
    fn braver() -> LimitBreak {
        LimitBreak::new(128, ActionSpec::limit_strike(48), 140)
    }

    /// Healing Wind: half of an ally's max HP.
    fn healing_wind() -> LimitBreak {
        LimitBreak::new(142, ActionSpec::limit_heal(16), 200)
    }

    /// Cure2 (1, 24 MP), Cure (0, 5 MP), Life (7, 34 MP), Fire (27, 4 MP): in this order, so that the
    /// first healing spell is not the cheapest.
    fn spells() -> Vec<Spell> {
        vec![
            Spell::new(1, ActionSpec::cure_spell(10, 24)),
            Spell::new(0, ActionSpec::cure_spell(5, 5)),
            Spell::new(7, ActionSpec::revive_spell(8, 34)),
            Spell::new(27, ActionSpec::damage_spell(8, 100, element::FIRE, 4)),
        ]
    }

    /// Potion (0), Ether (3), Megalixir (6, the whole party), Phoenix Down (7) and Grenade (9, damages an enemy).
    fn items() -> Arc<ItemTable> {
        Arc::new(ItemTable::new([
            (0, ItemData::new(ActionSpec::flat_restore_item(5, false))),
            (3, ItemData::new(ActionSpec::flat_restore_item(5, true))),
            (6, ItemData::new(ActionSpec::full_restore_item().with_aim(Aim::Side))),
            (7, ItemData::new(ActionSpec::revive_spell(8, 0).with_aim(Aim::One))),
            (9, ItemData::new(ActionSpec::damage_spell(8, 255, 0, 0).with_aim(Aim::One))),
        ]))
    }
}

impl BattleRules for Scenario {
    fn build(&self, setup: &BattleSetup) -> Result<BattleState, BattleBuildError> {
        if setup.party.is_empty() {
            return Err(BattleBuildError::EmptyParty);
        }
        let party = (0..setup.party.len())
            .map(|i| {
                let mut c = Combatant::new(format!("Hero {i}"), 6, MAX_HP);
                c.attack = 40;
                c.defense = 5;
                c.magic = 20;
                c.magic_defense = 5;
                c.dex = 40;
                c.accuracy = 100;
                c.max_mp = MAX_MP;
                c.mp = MAX_MP;
                c.spells = self.spells.clone();
                c.limit_break = self.limit;
                c.limit = setup.party[i].limit;
                c.all_left = self.all;
                if i == 0 {
                    c.hp = self.hp;
                    c.mp = self.mp;
                }
                if i == 1 && self.ally_dead {
                    c.hp = 0;
                }
                c
            })
            .collect();
        // A weak hitter: nobody dies while a test looks at something else.
        let mut enemy = Combatant::new("Dummy", 3, self.enemy_hp);
        enemy.attack = 1;
        enemy.defense = 0;
        enemy.dex = 40;
        enemy.accuracy = 100;
        enemy.reward = Reward { exp: 12, gil: 20, ap: 2 };
        enemy.actions = self.enemy_attacks.clone();
        Ok(BattleState::with_config(setup.seed, party, vec![enemy], setup.config()).with_items(Self::items()).with_escape(self.escape))
    }

    fn data_fingerprint(&self) -> Fingerprint {
        SyntheticRules::FINGERPRINT
    }

    fn formation(&self, _id: u16) -> Option<FormationInfo> {
        Some(FormationInfo { preemptive: true, back_attack: false, enemies: 1 })
    }

    fn starting_level(&self, _character: u8) -> u8 {
        6
    }

    fn default_formation(&self) -> Option<u16> {
        Some(0)
    }
}

/// A server for one battle: builds the engine like the real one and produces the messages.
struct Sim {
    id: BattleId,
    setup: BattleSetup,
    state: BattleState,
}

impl Sim {
    /// A battle of `formation` for `accounts` (member `i` is `ActorId(i)`, the enemy follows them).
    fn new(rules: &dyn BattleRules, formation: u16, accounts: &[AccountId], preemptive: bool) -> Self {
        Self::with_limits(rules, formation, accounts, preemptive, &[])
    }

    /// Like [`Sim::new`], with member `i` starting with the limit gauge `limits[i]` (0 when not listed).
    fn with_limits(rules: &dyn BattleRules, formation: u16, accounts: &[AccountId], preemptive: bool, limits: &[u16]) -> Self {
        let party = accounts
            .iter()
            .enumerate()
            .map(|(i, &account)| BattleMember {
                entity: EntityId(100 + i as u64),
                account,
                character: i as u8,
                level: 6,
                weapon: None,
                armor: None,
                limit: limits.get(i).copied().unwrap_or(0),
            })
            .collect();
        let setup = BattleSetup { formation_id: formation, seed: 7, party, preemptive, back_attack: false, battle_speed: 0, ticks_per_second: 1000 };
        let state = rules.build(&setup).expect("a battle");
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

    /// The server applies `command` of `actor` and says so (its engine must take it).
    fn command(&mut self, actor: ActorId, command: Command) -> ServerMsg {
        let events = self.state.submit_command(actor, command).expect("the server's engine takes the command");
        ServerMsg::BattleEvent { battle_id: self.id, tick: self.state.tick(), events, command: Some(AppliedCommand { actor, command }) }
    }

    /// Let time pass, telling `c` whatever happens, until `done` or the battle is over.
    fn run_until(&mut self, c: &mut BattleClient, t: &mut f64, done: impl Fn(&BattleState) -> bool) {
        for _ in 0..200_000 {
            if done(&self.state) || self.state.outcome().is_some() {
                return;
            }
            *t += 0.005;
            let events = self.state.step_ticks(5);
            if !events.is_empty() {
                c.on_message(*t, ServerMsg::BattleEvent { battle_id: self.id, tick: self.state.tick(), events, command: None });
            }
        }
        panic!("the situation never came");
    }

    /// Until our character (actor 0) can act again.
    fn until_our_turn(&mut self, c: &mut BattleClient, t: &mut f64) {
        self.run_until(c, t, |s| s.ready_actors().contains(&ActorId(0)));
    }

    /// The end of the won battle: the split rewards, and `drops` for our account (the first member).
    fn end(&self, drops: &[InventoryEntry]) -> ServerMsg {
        let outcome = self.state.outcome().expect("over");
        let roster: Vec<(AccountId, bool)> =
            self.setup.party.iter().enumerate().map(|(i, m)| (m.account, self.state.actor(ActorId(i as u8)).is_some_and(Combatant::is_alive))).collect();
        let mut rewards = if outcome == Outcome::Victory { split_rewards(self.state.rewards(), &roster) } else { vec![] };
        if let Some(ours) = rewards.first_mut() {
            ours.items = drops.to_vec();
        }
        ServerMsg::BattleEnd { battle_id: self.id, outcome, rewards }
    }
}

/// A client of ours that mirrors `rules`.
fn client(rules: Arc<dyn BattleRules>) -> BattleClient {
    let mut c = BattleClient::new(ME);
    c.set_rules(Some(rules));
    c
}

/// A name book that knows what the scenario's ids are, and one enemy attack.
fn names() -> Arc<NameBook> {
    let table = |len: usize, named: &[(usize, &str)]| {
        let mut all = vec![String::new(); len];
        for (i, name) in named {
            all[*i] = (*name).to_string();
        }
        all
    };
    let spells = table(143, &[(0, "Cure"), (1, "Cure2"), (7, "Life"), (27, "Fire"), (128, "Braver"), (142, "Healing Wind")]);
    let items = table(10, &[(0, "Potion"), (3, "Ether"), (7, "Phoenix Down"), (9, "Grenade")]);
    Arc::new(NameBook::from_tables(&spells, &items).with_enemy_attack(75, 272, "Machine Gun"))
}

fn notes(c: &mut BattleClient) -> String {
    c.drain_notes().join("\n")
}

// ----- driving the menu -------------------------------------------------------------------------

fn menu(c: &BattleClient, t: f64) -> menu::MenuView {
    c.view(t).and_then(|v| v.menu).expect("our menu is open")
}

/// Move the command cursor to `command` and press Confirm.
fn confirm_command(c: &mut BattleClient, command: usize, t: f64) -> MenuReply {
    for _ in 0..4 {
        if menu(c, t).command_cursor == command {
            break;
        }
        c.menu_key(MenuKey::Down, t);
    }
    c.menu_key(MenuKey::Confirm, t)
}

/// In an open Magic or Item list: move to `row` and press Confirm.
fn confirm_row(c: &mut BattleClient, row: usize, t: f64) -> MenuReply {
    for _ in 0..64 {
        let list = menu(c, t).list.expect("a list is open");
        if list.scroll + list.cursor == row {
            break;
        }
        c.menu_key(MenuKey::Down, t);
    }
    c.menu_key(MenuKey::Confirm, t)
}

/// With the target pointer open: move it to `target` and press Confirm.
fn confirm_target(c: &mut BattleClient, target: ActorId, t: f64) -> MenuReply {
    for _ in 0..8 {
        if menu(c, t).aim.expect("the pointer is open").one() == Some(target) {
            break;
        }
        c.menu_key(MenuKey::Right, t);
    }
    c.menu_key(MenuKey::Confirm, t)
}

/// Make a choice with the keys, as the player does: the command, then the list row if it has a list, then
/// the target if it aims. Returns what the client says to send.
fn choose(c: &mut BattleClient, t: f64, command: usize, row: Option<usize>, target: Option<ActorId>) -> BattleCmd {
    let mut reply = confirm_command(c, command, t);
    if let Some(row) = row {
        assert_eq!(reply, MenuReply::Handled, "the command opens its list");
        reply = confirm_row(c, row, t);
    }
    if let Some(target) = target {
        assert_eq!(reply, MenuReply::Handled, "the choice opens the target pointer");
        reply = confirm_target(c, target, t);
    }
    match reply {
        MenuReply::Send(cmd) => cmd,
        other => panic!("the menu sent nothing: {other:?}"),
    }
}

/// Fight on with the bot's rule until the battle is won (the enemies are weak hitters and ours is a healthy
/// character, so the rule is a plain attack every time). Returns how many commands it sent.
fn fight_to_the_end(sim: &mut Sim, c: &mut BattleClient, t: &mut f64) -> u64 {
    // One command per round: the round that finds the battle over has sent as many as it has counted.
    for sent in 0..500_u64 {
        *t += 0.01;
        sim.until_our_turn(c, t);
        if sim.state.outcome().is_some() {
            return sent;
        }
        let next = c.bot_command(*t).expect("our turn");
        assert!(matches!(next, BattleCmd::Command { command: Command::Attack { .. }, .. }), "healthy, with the whole battle ahead: it attacks: {next:?}");
        send(sim, c, next, *t);
    }
    panic!("the battle never ends");
}

fn row_of_spell(c: &mut BattleClient, spell: u16) -> usize {
    c.menu_model_mut().spells.iter().position(|s| s.id == spell).expect("the spell is listed")
}

fn row_of_item(c: &mut BattleClient, item: u16) -> usize {
    c.menu_model_mut().items.iter().position(|i| i.id == item).expect("the item is listed")
}

/// What the window does with a request it sends (the client remembers it as in flight), and then what the
/// server does: apply it and tell the client. Returns the server's message.
fn send(sim: &mut Sim, c: &mut BattleClient, cmd: BattleCmd, t: f64) -> ServerMsg {
    let BattleCmd::Command { battle_id, actor, command } = cmd else { panic!("not a command: {cmd:?}") };
    assert_eq!(battle_id, sim.id);
    c.note_sent(&cmd, t);
    let msg = sim.command(actor, command);
    c.on_message(t, msg.clone());
    msg
}

fn events_of(msg: &ServerMsg) -> &[BattleEvent] {
    match msg {
        ServerMsg::BattleEvent { events, .. } => events,
        other => panic!("not an event message: {other:?}"),
    }
}

/// The numbers floating up over the fighters once the blows of the events that arrived at `t` have
/// landed (a number is born when its blow lands, at most `MAX_IMPACT_DELAY` after the arrival).
fn popups_over(c: &BattleClient, t: f64) -> Vec<(u8, PopupKind)> {
    let landed = t + crate::battle::scene::cue::MAX_IMPACT_DELAY;
    c.view(landed).expect("a battle").popups.iter().map(|p| (p.target.0, p.kind)).collect()
}

fn banner(c: &BattleClient, t: f64) -> Option<String> {
    c.view(t).and_then(|v| v.banner).map(|b| b.text)
}

// ----- the menu model -------------------------------------------------------------------------------

#[test]
fn the_menu_model_is_filled_from_the_engine_at_the_start_and_from_the_inventory_as_it_arrives() {
    let rules: Arc<dyn BattleRules> = Arc::new(SyntheticRules::default());
    let sim = Sim::new(&*rules, 3, &[ME], true);
    let mut c = client(rules);
    assert_eq!(*c.menu_model_mut(), BattleMenuModel::default(), "nothing to offer before a battle");

    // SyntheticRules' fighters know Cure (0, 5 MP), Life (7, 34 MP) and Fire (27, 4 MP): listed with their
    // costs and the side they aim at, named plainly without a book, none of them switched off (the menu
    // compares the cost with the MP it is given), and Defend exists. The server has not said what we carry.
    c.on_message(1.0, sim.start());
    assert_eq!(
        c.menu_model_mut().spells,
        [
            MenuSpell::new(0, "Spell 0", 5, TargetRule::Allies).with_reach(Reach::OneOrAll),
            MenuSpell::new(7, "Spell 7", 34, TargetRule::Allies).with_reach(Reach::OneOrAll),
            MenuSpell::new(27, "Spell 27", 4, TargetRule::Enemies).with_reach(Reach::OneOrAll),
        ],
        "Cure, Life and Fire can hit a whole side, and the fighter has one All use to do it with"
    );
    assert_eq!(c.menu_model_mut().all_left, 1);
    assert!(c.menu_model_mut().spells.iter().all(|s| s.enabled), "the live MP is the menu's business, not baked in at the start");
    assert!(c.menu_model_mut().defend);
    assert!(c.menu_model_mut().items.is_empty(), "no inventory message yet");
    let flags = |c: &BattleClient| menu(c, 1.0).commands.iter().map(|r| (r.label, r.enabled)).collect::<Vec<_>>();
    assert_eq!(flags(&c), [("Attack", true), ("Magic", true), ("Item", false), ("Defend", true), ("Escape", true)]);

    // The inventory arrives: what the engine can use in battle is listed in the order sent with its count
    // and whom it aims at (a revival at the fallen); an id the engine cannot use is not listed at all.
    c.on_inventory(1.5, vec![InventoryEntry::new(0, 3), InventoryEntry::new(3, 1), InventoryEntry::new(7, 1), InventoryEntry::new(99, 5)]);
    assert_eq!(
        c.menu_model_mut().items,
        [
            MenuItem::new(0, "Item 0", 3, TargetRule::Allies),
            MenuItem::new(3, "Item 3", 1, TargetRule::Allies),
            MenuItem::new(7, "Item 7", 1, TargetRule::DeadAllies),
        ]
    );
    assert_eq!(flags(&c), [("Attack", true), ("Magic", true), ("Item", true), ("Defend", true), ("Escape", true)]);

    // The same lists carry this machine's names once it has them (the next message renames them).
    c.set_names(Some(names()));
    c.on_inventory(2.0, vec![InventoryEntry::new(0, 2), InventoryEntry::new(9, 4)]);
    assert_eq!(c.menu_model_mut().items, [MenuItem::new(0, "Potion", 2, TargetRule::Allies)], "Grenade is not an item of this engine's table");
    assert_eq!(c.menu_model_mut().spells.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["Cure", "Life", "Fire"]);

    // The list is what the menu shows, row by row.
    assert_eq!(c.menu_key(MenuKey::Down, 2.0), MenuReply::Handled);
    assert_eq!(c.menu_key(MenuKey::Confirm, 2.0), MenuReply::Handled);
    let rows: Vec<(String, String, bool)> = menu(&c, 2.0).list.expect("the Magic list").rows.into_iter().map(|r| (r.label, r.right, r.enabled)).collect();
    assert_eq!(rows, [("Cure".to_string(), "MP 5".to_string(), true), ("Life".to_string(), "MP 34".to_string(), true), ("Fire".to_string(), "MP 4".to_string(), true)], "60 MP pay for all three");
}

#[test]
fn every_kind_of_item_gets_its_target_rule_and_the_menu_greys_out_the_spells_the_mp_cannot_pay() {
    let rules: Arc<dyn BattleRules> = Arc::new(Scenario { mp: 20, ..Scenario::new() });
    let sim = Sim::new(&*rules, 0, &[ME, PEER], true);
    let mut c = client(rules);
    c.on_inventory(0.5, vec![InventoryEntry::new(0, 3), InventoryEntry::new(3, 1), InventoryEntry::new(7, 1), InventoryEntry::new(9, 2)]);
    // The inventory is the account's: it was told before the battle began and the battle's menu has it at once.
    c.on_message(1.0, sim.start());
    assert_eq!(
        c.menu_model_mut().items.iter().map(|i| (i.id, i.count, i.target)).collect::<Vec<_>>(),
        [(0, 3, TargetRule::Allies), (3, 1, TargetRule::Allies), (7, 1, TargetRule::DeadAllies), (9, 2, TargetRule::Enemies)]
    );
    // Our MP is what the menu compares the costs with: 20 pays Cure2 (24)? No. Cure (5) and Fire (4), yes.
    let rows: Vec<bool> = {
        c.menu_key(MenuKey::Down, 1.0);
        c.menu_key(MenuKey::Confirm, 1.0);
        menu(&c, 1.0).list.expect("the Magic list").rows.iter().map(|r| r.enabled).collect()
    };
    assert_eq!(rows, [false, true, false, true], "Cure2, Cure, Life, Fire with 20 MP");
}

#[test]
fn every_battle_start_replaces_what_the_last_one_left_in_the_model() {
    let rules: Arc<dyn BattleRules> = Arc::new(SyntheticRules::default());
    let mut c = client(rules.clone());
    c.on_inventory(0.0, vec![InventoryEntry::new(0, 3)]);
    let ours = Sim::new(&*rules, 3, &[ME], true);
    c.on_message(1.0, ours.start());
    assert_eq!(c.menu_model_mut().spells.len(), 3);
    assert_eq!(c.menu_model_mut().items.len(), 1);
    // Another battle starts (the first is dropped, which a server never does but a client survives): this
    // machine is not in its party, so there is nobody of ours to offer anything for, and only Attack is left.
    let mut theirs = Sim::new(&*rules, 3, &[PEER], true);
    theirs.id = BattleId(2);
    c.on_message(2.0, theirs.start());
    assert_eq!(*c.menu_model_mut(), BattleMenuModel::default(), "not in the party: the spells of the last battle are gone");
    // One this machine cannot rebuild has no engine to ask: the same.
    let mut broken = ours.start();
    if let ServerMsg::EncounterStart { battle_id, formation_id, .. } = &mut broken {
        *battle_id = BattleId(3);
        *formation_id = 99;
    }
    c.on_message(3.0, broken);
    assert_eq!(*c.menu_model_mut(), BattleMenuModel::default(), "no replay: nothing to offer");
    assert_eq!(c.menu_key(MenuKey::Confirm, 3.0), MenuReply::Ignored, "and no engine says it is our turn either");
    // And a battle of ours again fills it, the inventory being the account's and still there.
    let mut again = Sim::new(&*rules, 3, &[ME], true);
    again.id = BattleId(4);
    c.on_message(4.0, again.start());
    assert_eq!((c.menu_model_mut().spells.len(), c.menu_model_mut().items.len(), c.menu_model_mut().defend), (3, 1, true));
    // Without any battle data on this machine there is no replay at all: the same.
    let mut blind = BattleClient::new(ME);
    blind.set_rules(None);
    blind.on_inventory(0.0, vec![InventoryEntry::new(0, 3)]);
    blind.on_message(1.0, ours.start());
    assert_eq!(*blind.menu_model_mut(), BattleMenuModel::default());
    assert_eq!(blind.inventory().len(), 1, "it still keeps what the server said");
    assert_eq!(blind.menu_key(MenuKey::Confirm, 1.0), MenuReply::Ignored);
}

// ----- sending commands, applying them, and what shows -------------------------------------------------

#[test]
fn spells_items_and_defend_chosen_with_the_keys_reach_the_server_as_engine_commands_and_the_replay_takes_them() {
    let rules: Arc<dyn BattleRules> = Arc::new(Scenario { hp: 100, mp: 40, ally_dead: true, ..Scenario::new() });
    let mut sim = Sim::new(&*rules, 0, &[ME, PEER], true);
    let mut c = client(rules);
    c.set_names(Some(names()));
    c.on_inventory(0.0, vec![InventoryEntry::new(0, 3), InventoryEntry::new(3, 1), InventoryEntry::new(7, 1), InventoryEntry::new(9, 2)]);
    c.on_message(0.0, sim.start());
    let (me, peer, foe) = (ActorId(0), ActorId(1), ActorId(2));
    let cmd = |command: Command| BattleCmd::Command { battle_id: BattleId(1), actor: me, command };
    let mut t = 1.0;
    let no_divergence = |c: &mut BattleClient, what: &str| {
        let n = notes(c);
        assert!(c.divergences() == 0 && !n.contains("DIVERGENCE"), "{what}: {n}");
        assert!(!c.active().expect("a battle").diverged(), "{what}");
    };

    // Fire at the dummy: a Magic command with the spell's id, costing its MP, named in the banner.
    let row = row_of_spell(&mut c, 27);
    let fire = choose(&mut c, t, MAGIC, Some(row), Some(foe));
    assert_eq!(fire, cmd(Command::Magic { spell: 27, target: Target::One(foe) }));
    let msg = send(&mut sim, &mut c, fire, t);
    no_divergence(&mut c, "Fire");
    assert!(events_of(&msg).contains(&BattleEvent::Action { actor: me, action: fmv_protocol::ActionRef::Magic(27) }));
    assert!(events_of(&msg).contains(&BattleEvent::MpSpent { actor: me, amount: 4 }));
    assert_eq!(banner(&c, t).as_deref(), Some("Fire"));
    assert!(popups_over(&c, t).iter().any(|(who, kind)| *who == foe.0 && matches!(kind, PopupKind::Damage { .. } | PopupKind::Miss)));
    assert_eq!(c.view(t).expect("a battle").party[0].mp, 36, "the caster's MP bar shows what the spell cost");

    // Cure on ourselves: the heal floats up in green over us, with the amount the engine healed.
    t += 2.0;
    sim.until_our_turn(&mut c, &mut t);
    t += 2.0;
    let row = row_of_spell(&mut c, 0);
    let cure = choose(&mut c, t, MAGIC, Some(row), Some(me));
    assert_eq!(cure, cmd(Command::Magic { spell: 0, target: Target::One(me) }), "Cure aims at the living party");
    let msg = send(&mut sim, &mut c, cure, t);
    no_divergence(&mut c, "Cure");
    let healed = events_of(&msg).iter().find_map(|e| if let BattleEvent::Heal { target, amount, .. } = e { Some((*target, *amount)) } else { None });
    let (who, amount) = healed.expect("Cure heals");
    assert!(who == me && amount > 0, "we were hurt, so something was restored");
    assert!(popups_over(&c, t).contains(&(0, PopupKind::Heal { amount })));
    assert_eq!(banner(&c, t).as_deref(), Some("Cure"));

    // Ether on ourselves: our MP comes back, in blue; the server then says it took one Ether from us, and the
    // menu's list (now without Ether, there was only one) follows.
    t += 2.0;
    sim.until_our_turn(&mut c, &mut t);
    t += 2.0;
    let row = row_of_item(&mut c, 3);
    let ether = choose(&mut c, t, ITEM, Some(row), Some(me));
    assert_eq!(ether, cmd(Command::Item { item: 3, target: Target::One(me) }));
    let msg = send(&mut sim, &mut c, ether, t);
    no_divergence(&mut c, "Ether");
    let restored = events_of(&msg).iter().find_map(|e| if let BattleEvent::MpRestored { amount, .. } = e { Some(*amount) } else { None });
    assert!(restored.is_some_and(|a| a > 0), "we had spent MP");
    assert!(popups_over(&c, t).contains(&(0, PopupKind::Mp { amount: restored.unwrap_or(0) })));
    assert_eq!(banner(&c, t).as_deref(), Some("Ether"));
    c.on_inventory(t, vec![InventoryEntry::new(0, 3), InventoryEntry::new(7, 1), InventoryEntry::new(9, 2)]);
    assert_eq!(c.menu_model_mut().items.iter().map(|i| i.id).collect::<Vec<_>>(), [0, 7, 9], "the last Ether is gone from the list");

    // Phoenix Down on the fallen second member: only the dead can be aimed at, and it comes back with the
    // HP the engine says, floating up in green over it.
    t += 2.0;
    sim.until_our_turn(&mut c, &mut t);
    t += 2.0;
    assert!(!sim.state.actor(peer).expect("the ally").is_alive());
    let row = row_of_item(&mut c, 7);
    assert_eq!(confirm_command(&mut c, ITEM, t), MenuReply::Handled);
    assert_eq!(confirm_row(&mut c, row, t), MenuReply::Handled);
    assert_eq!(menu(&c, t).aim.expect("the pointer").one(), Some(peer), "the pointer starts on the one who fell");
    c.menu_key(MenuKey::Right, t);
    assert_eq!(menu(&c, t).aim.expect("the pointer").one(), Some(peer), "and has nobody else to go to");
    let MenuReply::Send(phoenix) = c.menu_key(MenuKey::Confirm, t) else { panic!("a command") };
    assert_eq!(phoenix, cmd(Command::Item { item: 7, target: Target::One(peer) }));
    let msg = send(&mut sim, &mut c, phoenix, t);
    no_divergence(&mut c, "Phoenix Down");
    let hp = events_of(&msg).iter().find_map(|e| if let BattleEvent::Revive { target, hp, .. } = e { Some((*target, *hp)) } else { None });
    let (who, hp) = hp.expect("Phoenix Down revives");
    assert!(who == peer && hp > 0);
    assert!(popups_over(&c, t).contains(&(1, PopupKind::Heal { amount: hp })), "a revival floats the HP it came back with");
    assert_eq!(banner(&c, t).as_deref(), Some("Phoenix Down"));
    let mirror = c.active().and_then(ActiveBattle::replay).expect("the replay");
    assert!(mirror.state().actor(peer).is_some_and(Combatant::is_alive), "the mirror brought it back too");
    c.on_inventory(t, vec![InventoryEntry::new(0, 3), InventoryEntry::new(9, 2)]);

    // A Grenade is aimed at the enemy, not at the party.
    t += 2.0;
    sim.until_our_turn(&mut c, &mut t);
    t += 2.0;
    let row = row_of_item(&mut c, 9);
    let grenade = choose(&mut c, t, ITEM, Some(row), Some(foe));
    assert_eq!(grenade, cmd(Command::Item { item: 9, target: Target::One(foe) }));
    send(&mut sim, &mut c, grenade, t);
    no_divergence(&mut c, "Grenade");
    assert_eq!(banner(&c, t).as_deref(), Some("Grenade"));

    // Defend: one key press after the command, no target, nothing floats up, and the mirror guards.
    t += 2.0;
    sim.until_our_turn(&mut c, &mut t);
    t += 2.0;
    let defend = choose(&mut c, t, DEFEND, None, None);
    assert_eq!(defend, cmd(Command::Defend));
    let msg = send(&mut sim, &mut c, defend, t);
    no_divergence(&mut c, "Defend");
    assert_eq!(events_of(&msg), [BattleEvent::Action { actor: me, action: fmv_protocol::ActionRef::Defend }]);
    assert_eq!(banner(&c, t).as_deref(), Some("Defend"));
    assert!(popups_over(&c, t).is_empty());
    assert!(c.active().and_then(ActiveBattle::replay).expect("the replay").state().is_defending(me));

    // Fight on with the bot's rule to the end: the rest is attacks, and the win is verified, drops and all.
    let attacks = fight_to_the_end(&mut sim, &mut c, &mut t);
    no_divergence(&mut c, "the rest of the battle");
    let drops = [InventoryEntry::new(3, 2), InventoryEntry::new(0, 1)];
    t += 0.5;
    c.on_message(t, sim.end(&drops));
    assert!(!c.in_battle() && c.victories() == 1);
    assert_eq!((c.verified(), c.divergences()), (1, 0), "the replay matched the whole log and the ending, whatever was dropped");
    let n = notes(&mut c);
    assert!(n.contains("verified: the server's") && !n.contains("NOT verified") && !n.contains("DIVERGENCE"), "{n}");
    assert!(n.contains("items=3:2,0:1)"), "the drops are in the note, by id and as sent: {n}");
    assert_eq!(c.commands_applied(), attacks + 6, "{n}");
    assert!(c.summary().contains(&format!("{} command(s) of ours applied ({attacks} attack, 2 magic, 3 item, 1 defend, 0 limit, 0 escape)", attacks + 6)), "{}", c.summary());
    // The panel lists what was dropped, named from this machine's book, in the order the server sent.
    let finale = c.view(t + 1.0).and_then(|v| v.finale).expect("the panel");
    let reward = finale.reward.expect("we are in the party");
    assert_eq!(reward.items, [panels::RewardItem { name: "Ether".into(), count: 2 }, panels::RewardItem { name: "Potion".into(), count: 1 }]);
}

#[test]
fn an_enemys_own_attack_is_named_in_the_banner_and_a_plain_attack_is_not() {
    let own = EnemyAction { id: 272, spec: ActionSpec::basic_attack(16, 100, true) };
    let rules: Arc<dyn BattleRules> = Arc::new(Scenario { enemy_attacks: vec![own], ..Scenario::new() });
    // Formation 301 is scene 75: the book names attack 272 of that scene.
    let mut sim = Sim::new(&*rules, 301, &[ME], true);
    let mut c = client(rules);
    c.set_names(Some(names()));
    c.on_message(0.0, sim.start());
    let mut t = 0.0;
    let mut seen = None;
    for _ in 0..100_000 {
        t += 0.005;
        let events = sim.state.step_ticks(5);
        if events.is_empty() {
            continue;
        }
        c.on_message(t, ServerMsg::BattleEvent { battle_id: sim.id, tick: sim.state.tick(), events, command: None });
        if let Some(text) = banner(&c, t) {
            seen = Some(text);
            break;
        }
    }
    assert_eq!(seen.as_deref(), Some("Machine Gun"));
    assert_eq!(c.divergences(), 0);

    // An enemy that has no attacks of its own attacks plainly: no banner, however often it does.
    let rules: Arc<dyn BattleRules> = Arc::new(Scenario::new());
    let mut sim = Sim::new(&*rules, 301, &[ME], true);
    let mut c = client(rules);
    c.set_names(Some(names()));
    c.on_message(0.0, sim.start());
    let mut t = 0.0;
    let mut hits = 0;
    for _ in 0..100_000 {
        t += 0.005;
        let events = sim.state.step_ticks(5);
        if events.is_empty() {
            continue;
        }
        hits += events.iter().filter(|e| matches!(e, BattleEvent::Action { action: fmv_protocol::ActionRef::Attack, .. })).count();
        c.on_message(t, ServerMsg::BattleEvent { battle_id: sim.id, tick: sim.state.tick(), events, command: None });
        assert_eq!(banner(&c, t), None, "a plain attack has no name");
        if hits >= 3 {
            break;
        }
    }
    assert!(hits >= 3, "the enemy attacked");
    assert_eq!(c.divergences(), 0);
}

#[test]
fn reward_items_are_named_listed_as_sent_and_noted_by_id_and_nothing_extra_is_said_without_drops() {
    // Without a book the items get plain labels; the order is the server's.
    let rules: Arc<dyn BattleRules> = Arc::new(Scenario { enemy_hp: 1, ..Scenario::new() });
    let mut sim = Sim::new(&*rules, 0, &[ME, PEER], true);
    let mut c = client(rules);
    c.on_message(0.0, sim.start());
    let mut t = 0.0;
    fight_to_the_end(&mut sim, &mut c, &mut t);
    assert_eq!(sim.state.outcome(), Some(Outcome::Victory));
    let mut end = sim.end(&[InventoryEntry::new(3, 2), InventoryEntry::new(0, 1), InventoryEntry::new(0, 0)]);
    // A zero stack and a repeated item are tidied like any server data; the peer's drops are not ours.
    if let ServerMsg::BattleEnd { rewards, .. } = &mut end {
        rewards[0].items.push(InventoryEntry::new(3, 1));
        rewards[1].items = vec![InventoryEntry::new(9, 9)];
    }
    c.on_message(t, end);
    let n = notes(&mut c);
    assert!(n.contains("(you earned 6 exp, 10 gil, 2 ap, items=3:3,0:1)"), "{n}");
    let reward = c.view(t + 0.5).and_then(|v| v.finale).and_then(|f| f.reward).expect("we are paid");
    assert_eq!(reward.items, [panels::RewardItem { name: "Item 3".into(), count: 3 }, panels::RewardItem { name: "Item 0".into(), count: 1 }]);
    assert_eq!(c.verified(), 1, "items are not part of what a replay can check");

    // A win that dropped nothing is said as it always was.
    let rules: Arc<dyn BattleRules> = Arc::new(Scenario { enemy_hp: 1, ..Scenario::new() });
    let mut sim = Sim::new(&*rules, 0, &[ME], true);
    let mut c = client(rules);
    c.on_message(0.0, sim.start());
    let mut t = 0.0;
    fight_to_the_end(&mut sim, &mut c, &mut t);
    c.on_message(t, sim.end(&[]));
    let n = notes(&mut c);
    assert!(n.contains("(you earned 12 exp, 20 gil, 2 ap);") && !n.contains("items="), "{n}");
    assert!(c.view(t + 0.5).and_then(|v| v.finale).and_then(|f| f.reward).is_some_and(|r| r.items.is_empty()));
}

// ----- the inventory ------------------------------------------------------------------------------

#[test]
fn an_inventory_message_is_kept_counted_and_noted_in_one_parseable_line() {
    let mut c = BattleClient::new(ME);
    assert_eq!((c.inventory().len(), c.inventory_total(), c.inventory_updates()), (0, 0, 0), "nothing is known until the server says");
    c.on_inventory(1.0, vec![InventoryEntry::new(0, 3), InventoryEntry::new(3, 1), InventoryEntry::new(7, 1)]);
    assert_eq!(c.inventory(), [InventoryEntry::new(0, 3), InventoryEntry::new(3, 1), InventoryEntry::new(7, 1)]);
    assert_eq!((c.inventory_total(), c.inventory_updates()), (5, 1));
    c.on_inventory(2.0, vec![InventoryEntry::new(7, 1), InventoryEntry::new(0, 2)]);
    assert_eq!(c.inventory(), [InventoryEntry::new(7, 1), InventoryEntry::new(0, 2)], "the last message is the whole inventory: nothing is merged with the one before");
    c.on_inventory(3.0, vec![]);
    assert_eq!((c.inventory().len(), c.inventory_total(), c.inventory_updates()), (0, 0, 3));
    assert_eq!(
        c.drain_notes(),
        [
            "inventory: stacks=3 total=5 items=0:3,3:1,7:1",
            "inventory: stacks=2 total=3 items=7:1,0:2",
            "inventory: stacks=0 total=0 items=-",
        ]
    );
    // Said between battles it changes no menu (there is none to change), and the messages that are not
    // about us leave it alone.
    c.on_message(4.0, ServerMsg::Pong { nonce: 1, tick: 1 });
    assert_eq!(c.inventory_updates(), 3);
}

/// The real order of play: the server tells the inventory once on joining, long before any battle
/// starts. Starting a battle rebuilds the command menu for the spells; it must keep those items.
#[test]
fn the_inventory_told_on_joining_fills_the_item_menu_of_a_later_battle() {
    let rules: Arc<dyn BattleRules> = Arc::new(Scenario::new());
    let sim = Sim::new(&*rules, 0, &[ME], true);
    let mut c = client(rules);
    c.on_inventory(0.5, vec![InventoryEntry::new(0, 3), InventoryEntry::new(3, 1)]);
    assert!(c.menu_model_mut().items.is_empty(), "no battle yet, so no menu to fill");
    c.on_message(1.0, sim.start());
    let items: Vec<(u16, u16)> = c.menu_model_mut().items.iter().map(|i| (i.id, i.count)).collect();
    assert_eq!(items, [(0, 3), (3, 1)], "the stock told on joining is in the first battle's Item list");
    assert!(c.menu_model_mut().defend, "Defend is offered");
}

#[test]
fn a_hostile_inventory_is_bounded_tidy_and_never_panics() {
    let rules: Arc<dyn BattleRules> = Arc::new(Scenario::new());
    let sim = Sim::new(&*rules, 0, &[ME], true);
    let mut c = client(rules);
    // Also while a battle runs: the refresh of the menu has to survive it too.
    c.on_message(0.0, sim.start());
    let _ = notes(&mut c);

    // A thousand different items: the first MAX_INVENTORY_STACKS are kept, the rest are not.
    c.on_inventory(1.0, (0..1000).map(|i| InventoryEntry::new(i, 1)).collect());
    assert_eq!(c.inventory().len(), MAX_INVENTORY_STACKS);
    assert_eq!(c.inventory().first(), Some(&InventoryEntry::new(0, 1)));
    assert_eq!(c.inventory().last(), Some(&InventoryEntry::new(MAX_INVENTORY_STACKS as u16 - 1, 1)));
    assert_eq!(c.inventory_total(), MAX_INVENTORY_STACKS as u32);
    // The engine can use five of them (Potion, Ether, Megalixir, Phoenix Down, Grenade): only those are in the menu.
    assert_eq!(c.menu_model_mut().items.iter().map(|i| i.id).collect::<Vec<_>>(), [0, 3, 6, 7, 9]);
    let n = notes(&mut c);
    assert_eq!(n.lines().count(), 1, "one note per message, however much it holds: {n}");
    assert!(n.starts_with("inventory: stacks=320 total=320 items=0:1,1:1,2:1,"), "{n}");

    // Duplicates are merged into the first place, the count saturating instead of wrapping round.
    c.on_inventory(2.0, vec![InventoryEntry::new(9, 60_000), InventoryEntry::new(3, 1), InventoryEntry::new(9, 60_000)]);
    assert_eq!(c.inventory(), [InventoryEntry::new(9, u16::MAX), InventoryEntry::new(3, 1)]);
    assert_eq!(c.inventory_total(), u32::from(u16::MAX) + 1);
    assert_eq!(c.menu_model_mut().items.iter().map(|i| (i.id, i.count)).collect::<Vec<_>>(), [(9, u16::MAX), (3, 1)]);
    c.on_inventory(2.5, (0..5_000).map(|_| InventoryEntry::new(3, 1)).collect());
    assert_eq!(c.inventory(), [InventoryEntry::new(3, 5_000)]);

    // Stacks of zero are not stacks.
    c.on_inventory(3.0, vec![InventoryEntry::new(0, 0), InventoryEntry::new(7, 2), InventoryEntry::new(3, 0)]);
    assert_eq!(c.inventory(), [InventoryEntry::new(7, 2)]);
    c.on_inventory(3.5, vec![InventoryEntry::new(0, 0)]);
    assert!(c.inventory().is_empty());
    assert!(c.menu_model_mut().items.is_empty());
    assert!(notes(&mut c).ends_with("items=-"));

    // The widest an honest inventory can be, every stack at its largest, adds up inside a u32.
    c.on_inventory(4.0, (0..MAX_INVENTORY_STACKS as u16).map(|i| InventoryEntry::new(i, u16::MAX)).collect());
    assert_eq!(c.inventory_total(), MAX_INVENTORY_STACKS as u32 * u32::from(u16::MAX));
    // Everything at once, over and over: no panic, the notes stay a bounded queue, and the menu still works.
    let junk: Vec<InventoryEntry> = (0..20_000u32)
        .map(|i| InventoryEntry::new((i * 7919 % 4_099) as u16, [0, 40_000, u16::MAX][(i % 3) as usize]))
        .collect();
    for round in 0..200 {
        c.on_inventory(5.0 + f64::from(round), junk.clone());
        assert!(c.inventory().len() <= MAX_INVENTORY_STACKS);
        assert!(c.inventory().iter().all(|e| e.count > 0));
        let mut seen: Vec<u16> = c.inventory().iter().map(|e| e.item).collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), c.inventory().len(), "one stack per item");
    }
    assert!(c.drain_notes().len() <= MAX_NOTES);
    assert_eq!(c.menu_key(MenuKey::Down, 300.0), MenuReply::Handled);
    // With no battle at all the same message is kept and noted and touches no menu.
    let mut idle = BattleClient::new(ME);
    idle.on_inventory(0.0, junk);
    assert!(idle.inventory().len() <= MAX_INVENTORY_STACKS && *idle.menu_model_mut() == BattleMenuModel::default());
}

// ----- the bot ----------------------------------------------------------------------------------------

/// A bot's situation: a one-member party at `hp` HP and `mp` MP against one dummy (actor 1), our gauge full.
fn bot_battle(scenario: Scenario) -> (Sim, BattleClient) {
    let rules: Arc<dyn BattleRules> = Arc::new(scenario);
    let sim = Sim::new(&*rules, 0, &[ME], true);
    let mut c = client(rules);
    c.on_message(0.0, sim.start());
    (sim, c)
}

fn command(c: &BattleClient, t: f64) -> Option<Command> {
    c.bot_command(t).map(|cmd| match cmd {
        BattleCmd::Command { battle_id, actor, command } => {
            assert_eq!((battle_id, actor), (BattleId(1), ActorId(0)));
            command
        }
        other => panic!("{other:?}"),
    })
}

const ME_AT_0: ActorId = ActorId(0);
const THE_DUMMY: ActorId = ActorId(1);

#[test]
fn a_hurt_bot_that_can_pay_heals_itself_with_the_cheapest_healing_spell() {
    // 50 of 200 HP is a quarter. Cure2 (24 MP, listed first), Cure (5 MP) and Life (34 MP, a revival) are
    // all payable with 40 MP: the cheapest healer is Cure, on itself.
    let (_sim, c) = bot_battle(Scenario { hp: 50, mp: 40, ..Scenario::new() });
    assert_eq!(command(&c, 1.0), Some(Command::Magic { spell: 0, target: Target::One(ME_AT_0) }));
    // With 24 MP Cure2 is payable too, and Cure is still the cheapest; with 5, only Cure is payable.
    for mp in [24, 5] {
        let (_sim, c) = bot_battle(Scenario { hp: 50, mp, ..Scenario::new() });
        assert_eq!(command(&c, 1.0), Some(Command::Magic { spell: 0, target: Target::One(ME_AT_0) }), "{mp} MP");
    }
    // Among equals the lowest spell id wins, whatever the order of the list.
    let twins = vec![Spell::new(9, ActionSpec::cure_spell(5, 5)), Spell::new(4, ActionSpec::cure_spell(5, 5))];
    let (_sim, c) = bot_battle(Scenario { hp: 50, spells: twins, ..Scenario::new() });
    assert_eq!(command(&c, 1.0), Some(Command::Magic { spell: 4, target: Target::One(ME_AT_0) }));
}

#[test]
fn a_hurt_bot_without_the_mp_attacks_and_so_does_one_that_is_well() {
    // Cure costs 5: with 4 MP (enough for Fire, which is no healer) the bot attacks the first living enemy.
    let (_sim, c) = bot_battle(Scenario { hp: 50, mp: 4, ..Scenario::new() });
    assert_eq!(command(&c, 1.0), Some(Command::Attack { target: THE_DUMMY }));
    let (_sim, c) = bot_battle(Scenario { hp: 1, mp: 0, ..Scenario::new() });
    assert_eq!(command(&c, 1.0), Some(Command::Attack { target: THE_DUMMY }));
    // Healthy: it attacks, MP or no MP.
    for hp in [200, 100, 61] {
        let (_sim, c) = bot_battle(Scenario { hp, ..Scenario::new() });
        assert_eq!(command(&c, 1.0), Some(Command::Attack { target: THE_DUMMY }), "{hp} HP");
    }
    // The line is under 30%: exactly 60 of 200 is not under it, 59 is.
    let (_sim, c) = bot_battle(Scenario { hp: 60, ..Scenario::new() });
    assert_eq!(command(&c, 1.0), Some(Command::Attack { target: THE_DUMMY }), "exactly 30%");
    let (_sim, c) = bot_battle(Scenario { hp: 59, ..Scenario::new() });
    assert_eq!(command(&c, 1.0), Some(Command::Magic { spell: 0, target: Target::One(ME_AT_0) }), "just under 30%");
}

#[test]
fn a_revival_is_never_a_heal_and_a_bot_that_knows_no_healer_attacks() {
    // Life (34 MP) is a restoring spell aimed at the party, but only at the fallen: with nothing else to heal
    // with, a hurt bot that stands attacks instead of casting it on itself.
    let life_and_fire = vec![Spell::new(7, ActionSpec::revive_spell(8, 34)), Spell::new(27, ActionSpec::damage_spell(8, 100, element::FIRE, 4))];
    let (_sim, c) = bot_battle(Scenario { hp: 10, spells: life_and_fire, ..Scenario::new() });
    assert_eq!(command(&c, 1.0), Some(Command::Attack { target: THE_DUMMY }));
    let (_sim, c) = bot_battle(Scenario { hp: 10, spells: vec![], ..Scenario::new() });
    assert_eq!(command(&c, 1.0), Some(Command::Attack { target: THE_DUMMY }));
}

#[test]
fn the_bot_acts_only_on_our_turn_with_a_replay_and_nothing_in_flight() {
    // No replay (no battle data on this machine): nothing to decide with.
    let mut blind = BattleClient::new(ME);
    blind.set_rules(None);
    let (sim, _) = bot_battle(Scenario::new());
    blind.on_message(0.0, sim.start());
    assert_eq!(blind.bot_command(1.0), None);
    // No battle at all, and a battle we are not in.
    assert_eq!(BattleClient::new(ME).bot_command(1.0), None);
    let rules: Arc<dyn BattleRules> = Arc::new(Scenario { hp: 10, ..Scenario::new() });
    let others = Sim::new(&*rules, 0, &[PEER], true);
    let mut watcher = client(rules);
    watcher.on_message(0.0, others.start());
    assert_eq!(watcher.bot_command(1.0), None, "the battle is not ours to play");
    // Not our turn: a normal start has nobody's gauge full.
    let rules: Arc<dyn BattleRules> = Arc::new(Scenario { hp: 10, ..Scenario::new() });
    let slow = Sim::new(&*rules, 0, &[ME], false);
    let mut c = client(rules);
    c.on_message(0.0, slow.start());
    assert_eq!(c.bot_command(1.0), None, "our gauge is not full");
    // Our turn, a command sent: nothing more until the answer or the timeout, then the same again.
    let (mut sim, mut c) = bot_battle(Scenario { hp: 10, ..Scenario::new() });
    let cmd = c.bot_command(1.0).expect("our turn");
    c.note_sent(&cmd, 1.0);
    assert_eq!(c.bot_command(1.0), None);
    assert_eq!(c.bot_command(1.0 + PENDING_TIMEOUT_S - 0.01), None);
    assert_eq!(c.bot_command(1.0 + PENDING_TIMEOUT_S), Some(cmd), "a lost command may be sent again");
    // The server applies it: our gauge is spent and the bot waits for the next one.
    let BattleCmd::Command { actor, command, .. } = cmd else { panic!("{cmd:?}") };
    c.on_message(1.1, sim.command(actor, command));
    assert_eq!(c.bot_command(1.2), None);
    // A refusal answers a command in flight too.
    let (_sim, mut c) = bot_battle(Scenario { hp: 10, ..Scenario::new() });
    let cmd = c.bot_command(1.0).expect("our turn");
    c.note_sent(&cmd, 1.0);
    c.on_message(1.1, ServerMsg::BattleRejected { battle_id: Some(BattleId(1)), reason: BattleRejection::NotEnoughMp });
    assert_eq!(c.bot_command(1.1), Some(cmd));
}

#[test]
fn the_headless_bot_sends_its_heal_and_then_attacks_once_it_is_well_or_out_of_mp() {
    use crate::party::PartyMirror;
    let (mut sim, mut c) = bot_battle(Scenario { hp: 50, mp: 40, enemy_hp: 5_000, ..Scenario::new() });
    let mut bot = BattleBot::new(true, false, 1);
    let party = PartyMirror::new(ME);
    let mut t = 1.0;
    let mut sent = Vec::new();
    for _ in 0..40 {
        let cmds = bot.step(t, &c, &party);
        assert!(cmds.len() <= 1);
        let Some(cmd) = cmds.into_iter().next() else {
            sim.until_our_turn(&mut c, &mut t);
            continue;
        };
        let BattleCmd::Command { command, .. } = cmd else { panic!("{cmd:?}") };
        sent.push(command);
        send(&mut sim, &mut c, cmd, t);
        t += 0.5;
        if matches!(command, Command::Attack { .. }) {
            break;
        }
        sim.until_our_turn(&mut c, &mut t);
    }
    assert_eq!(sent.first(), Some(&Command::Magic { spell: 0, target: Target::One(ME_AT_0) }), "{sent:?}");
    let attack_at = sent.iter().position(|c| matches!(c, Command::Attack { .. })).expect("it attacks once it is no longer in need and able to heal");
    assert!(sent[..attack_at].iter().all(|c| *c == Command::Magic { spell: 0, target: Target::One(ME_AT_0) }), "only Cure on itself until then: {sent:?}");
    // Why it stopped healing: it is above the line, or it cannot pay for another Cure.
    let me = c.active().and_then(ActiveBattle::replay).expect("the replay").state().actor(ME_AT_0).expect("us").clone();
    assert!(u64::from(me.hp) * 10 >= u64::from(me.max_hp) * 3 || me.mp < 5, "hp {} mp {}", me.hp, me.mp);
    assert_eq!(c.divergences(), 0, "{}", notes(&mut c));
}

// ----- limit breaks and escape (protocol v7) ------------------------------------------------------------

fn no_divergence(c: &mut BattleClient, what: &str) {
    let n = notes(c);
    assert!(c.divergences() == 0 && !n.contains("DIVERGENCE"), "{what}: {n}");
    assert!(!c.active().is_some_and(ActiveBattle::diverged), "{what}");
}

/// The events a command produced, without the `GaugeReady`s of the start that came with it (a preemptive
/// start announces the full gauges with the first message).
fn of_the_command(msg: &ServerMsg) -> Vec<BattleEvent> {
    events_of(msg).iter().copied().filter(|e| !matches!(e, BattleEvent::GaugeReady { .. })).collect()
}

/// A battle of one dummy that hits for 20 (so the gauge of a 200 HP Braver user moves by 54 a blow) against `accounts`.
fn limit_battle(scenario: Scenario, accounts: &[AccountId], limits: &[u16], preemptive: bool) -> (Sim, BattleClient) {
    let rules: Arc<dyn BattleRules> = Arc::new(Scenario { enemy_attacks: vec![Scenario::flat_blow()], ..scenario });
    let sim = Sim::with_limits(&*rules, 0, accounts, preemptive, limits);
    let mut c = client(rules);
    c.set_names(Some(names()));
    c.on_message(0.0, sim.start());
    (sim, c)
}

#[test]
fn the_party_panel_carries_every_members_gauge_and_the_menu_says_limit_once_a_blow_has_filled_ours() {
    let (mut sim, mut c) = limit_battle(Scenario { limit: Some(Scenario::braver()), ..Scenario::new() }, &[ME, PEER], &[100, 255], false);
    let view = c.view(0.5).expect("a battle");
    assert_eq!(view.party.iter().map(|b| b.limit).collect::<Vec<_>>(), [100, 255], "the gauges the server started them with");
    assert!(view.enemies.iter().all(|b| b.limit == 0), "an enemy has none");
    let limit = c.menu_model_mut().limit.clone().expect("our character has a limit");
    assert_eq!((limit.id, limit.name.as_str(), limit.target, limit.ready), (128, "Braver", TargetRule::Enemies, false));

    // Blows fill ours (54 each): the bar follows the replay, and the model follows it too, with no inventory
    // message to prompt it. At our next turn the first row says Limit.
    let mut t = 1.0;
    sim.run_until(&mut c, &mut t, |s| s.limit_ready(ME_AT_0));
    let seen = c.view(t).expect("a battle").party[0].limit;
    assert_eq!((seen, sim.state.limit_gauge(ME_AT_0)), (255, 255), "capped at a full gauge");
    assert!(c.menu_model_mut().limit.as_ref().is_some_and(|l| l.ready));
    sim.until_our_turn(&mut c, &mut t);
    let rows = menu(&c, t).commands.iter().map(|r| (r.label, r.enabled)).collect::<Vec<_>>();
    assert_eq!(rows, [("Limit", true), ("Magic", true), ("Item", false), ("Defend", true), ("Escape", true)]);
    no_divergence(&mut c, "the gauge filling");
}

#[test]
fn a_full_gauge_turns_attack_into_limit_the_server_applies_it_and_attack_comes_back() {
    let (mut sim, mut c) = limit_battle(Scenario { limit: Some(Scenario::braver()), ..Scenario::new() }, &[ME], &[255], true);
    let mut t = 1.0;
    assert_eq!(menu(&c, t).commands[0].label, "Limit");
    let enemy = ActorId(1);
    let cmd = choose(&mut c, t, 0, None, Some(enemy));
    assert_eq!(cmd, BattleCmd::Command { battle_id: BattleId(1), actor: ME_AT_0, command: Command::Limit { target: Target::One(enemy) } });
    let msg = send(&mut sim, &mut c, cmd, t);
    no_divergence(&mut c, "the limit");
    let events = of_the_command(&msg);
    assert_eq!(events[0], BattleEvent::Action { actor: ME_AT_0, action: fmv_protocol::ActionRef::Limit(128) });
    assert_eq!(events[1], BattleEvent::LimitGauge { actor: ME_AT_0, value: 0 });
    assert!(matches!(events[2], BattleEvent::Damage { actor: ME_AT_0, target, critical: false, .. } if target == enemy), "{events:?}");
    assert_eq!(c.view(t).expect("a battle").party[0].limit, 0, "the bar empties");
    // It was our one command: the next turn has Attack back where Limit was.
    t += 1.0;
    sim.until_our_turn(&mut c, &mut t);
    assert_eq!(menu(&c, t).commands[0].label, "Attack");
    assert!(!c.menu_model_mut().limit.as_ref().expect("still has one").ready);
}

#[test]
fn a_limit_that_wins_the_battle_ends_it_on_the_victory_and_is_counted_in_the_summary() {
    // Braver (about 140 damage here) against a dummy of 100 HP.
    let (mut sim, mut c) = limit_battle(Scenario { limit: Some(Scenario::braver()), enemy_hp: 100, ..Scenario::new() }, &[ME], &[255], true);
    let t = 1.0;
    let cmd = choose(&mut c, t, 0, None, Some(ActorId(1)));
    let msg = send(&mut sim, &mut c, cmd, t);
    no_divergence(&mut c, "the winning limit");
    assert_eq!(of_the_command(&msg).last(), Some(&BattleEvent::Victory), "the gauge was spent before the blow, so the victory is still the last event");
    c.on_message(t + 0.5, sim.end(&[]));
    assert_eq!((c.victories(), c.verified(), c.divergences()), (1, 1, 0));
    assert!(c.summary().contains("1 won") && c.summary().contains("1 limit, 0 escape)"), "{}", c.summary());
}

#[test]
fn a_healing_limit_aims_at_the_whole_party_through_the_real_menu() {
    let (mut sim, mut c) = limit_battle(Scenario { limit: Some(Scenario::healing_wind()), hp: 60, ..Scenario::new() }, &[ME, PEER], &[255, 0], true);
    let t = 1.0;
    assert_eq!(menu(&c, t).commands[0].label, "Limit");
    assert_eq!(c.menu_key(MenuKey::Confirm, t), MenuReply::Handled);
    // Healing Wind hits the whole party and nothing smaller: the pointer starts on ALL and the arrows have
    // nowhere else to take it.
    let aim = menu(&c, t).aim.expect("the pointer");
    assert_eq!((aim.at, aim.enemy, aim.reach), (AimAt::All, false, Reach::All));
    for key in [MenuKey::Right, MenuKey::Down, MenuKey::Left, MenuKey::Up] {
        assert_eq!(c.menu_key(key, t), MenuReply::Handled);
        assert_eq!(menu(&c, t).aim.expect("the pointer").at, AimAt::All, "{key:?}");
    }
    let cmd = match c.menu_key(MenuKey::Confirm, t) {
        MenuReply::Send(cmd) => cmd,
        other => panic!("{other:?}"),
    };
    assert_eq!(cmd, BattleCmd::Command { battle_id: BattleId(1), actor: ME_AT_0, command: Command::Limit { target: Target::AllAllies } });
    let msg = send(&mut sim, &mut c, cmd, t);
    no_divergence(&mut c, "Healing Wind");
    // Half of 200 max HP on a character at 60: 100; the other member is not hurt, so there is nothing to give it,
    // but the wind reaches it all the same (one event per member, in order; the replay checked both).
    let healed: Vec<(ActorId, u32)> = events_of(&msg)
        .iter()
        .filter_map(|e| if let BattleEvent::Heal { target, amount, .. } = e { Some((*target, *amount)) } else { None })
        .collect();
    assert_eq!(healed, [(ME_AT_0, 100), (ActorId(1), 0)], "{:?}", events_of(&msg));
}

#[test]
fn escape_is_the_last_row_and_a_successful_one_ends_the_battle_for_everyone_with_nothing_paid() {
    let (mut sim, mut c) = limit_battle(Scenario::new(), &[ME, PEER], &[], true);
    let t = 1.0;
    assert_eq!(menu(&c, t).commands[4], menu::CommandRow { label: "Escape", enabled: true });
    let cmd = choose(&mut c, t, 4, None, None);
    assert_eq!(cmd, BattleCmd::Command { battle_id: BattleId(1), actor: ME_AT_0, command: Command::Escape }, "no list and no pointer: it goes at once");
    let msg = send(&mut sim, &mut c, cmd, t);
    no_divergence(&mut c, "the escape");
    assert_eq!(of_the_command(&msg), [BattleEvent::Action { actor: ME_AT_0, action: fmv_protocol::ActionRef::Escape }], "a successful escape has no event of its own");
    assert_eq!(sim.state.outcome(), Some(Outcome::Escaped));
    // The server ends the battle: nothing is paid, and the client counts an escape (not a win or a loss) that
    // its own replay confirmed.
    c.on_message(t + 0.5, sim.end(&[]));
    assert!(!c.in_battle());
    assert_eq!((c.escapes(), c.victories(), c.defeats()), (1, 0, 0));
    assert_eq!((c.verified(), c.divergences()), (1, 0), "the replay reproduced the log and the ending, and the empty rewards");
    let n = notes(&mut c);
    assert!(n.contains("ended in Escaped") && n.contains("the party got away: nothing is paid") && n.contains("verified"), "{n}");
    assert!(!n.contains("earned"), "{n}");
    assert!(c.summary().contains("1 escaped") && c.summary().contains("0 won") && c.summary().contains("1 escape)"), "{}", c.summary());
    // The panel says so for the few seconds it lingers, with no reward on it.
    let finale = c.view(t + 1.5).and_then(|v| v.finale).expect("the panel");
    assert_eq!((finale.outcome, finale.reward), (Outcome::Escaped, None));
}

#[test]
fn a_failed_escape_costs_the_turn_only_and_a_boss_that_cannot_be_escaped_greys_the_row_out() {
    // Difficulty 4 and no sure checks: four successes in four checks is 0.4%, so seed 7 fails.
    let (mut sim, mut c) = limit_battle(Scenario { escape: Some(EscapeRule { difficulty: 4, sure: false }), ..Scenario::new() }, &[ME], &[], true);
    let mut t = 1.0;
    let cmd = choose(&mut c, t, 4, None, None);
    let msg = send(&mut sim, &mut c, cmd, t);
    no_divergence(&mut c, "the failed escape");
    assert_eq!(
        of_the_command(&msg),
        [BattleEvent::Action { actor: ME_AT_0, action: fmv_protocol::ActionRef::Escape }, BattleEvent::EscapeFailed { actor: ME_AT_0 }]
    );
    assert!(c.in_battle() && c.escapes() == 0, "the battle goes on");
    assert!(c.view(t).and_then(|v| v.menu).is_none(), "the turn is over: the menu closes until the gauge fills again");
    t += 1.0;
    sim.until_our_turn(&mut c, &mut t);
    assert!(menu(&c, t).commands[4].enabled, "it can try again");

    // A boss: the engine says it cannot be escaped, so the row is dim, says why and sends nothing.
    let (_sim, mut c) = limit_battle(Scenario { escape: None, ..Scenario::new() }, &[ME], &[], true);
    let v = menu(&c, 1.0);
    assert_eq!((v.commands[4].label, v.commands[4].enabled), ("Escape", false));
    assert!(!c.menu_model_mut().escape);
    assert_eq!(confirm_command(&mut c, 4, 1.0), MenuReply::Refused("Cannot escape from this battle"));
    assert_eq!(menu(&c, 1.0).hint, Some("Cannot escape from this battle"));
}

#[test]
fn the_servers_refusals_of_a_limit_or_an_escape_are_counted_and_noted_and_free_the_command() {
    let (_sim, mut c) = limit_battle(Scenario::new(), &[ME], &[], true);
    let cmd = BattleCmd::Command { battle_id: BattleId(1), actor: ME_AT_0, command: Command::Limit { target: Target::One(ActorId(1)) } };
    for (n, reason) in [BattleRejection::LimitNotReady, BattleRejection::NoLimit, BattleRejection::CannotEscape].into_iter().enumerate() {
        c.note_sent(&cmd, 1.0);
        assert_eq!(c.menu_key(MenuKey::Confirm, 1.1), MenuReply::Ignored, "the menu is closed while a command is in flight");
        c.on_message(1.2, ServerMsg::BattleRejected { battle_id: Some(BattleId(1)), reason });
        assert_eq!(c.rejections(), n as u64 + 1);
        assert_eq!(c.last_rejection(), Some(reason));
        assert!(notes(&mut c).contains(&format!("request refused: {reason}")));
        // The refusal freed the command: the menu takes Confirm again (and aims, as Attack does).
        assert_eq!(c.menu_key(MenuKey::Confirm, 1.3), MenuReply::Handled);
        assert_eq!(c.menu_key(MenuKey::Back, 1.3), MenuReply::Handled);
    }
}

#[test]
fn the_bot_gives_a_full_limit_but_heals_first_when_hurt_aims_a_healing_limit_at_the_wounded_and_never_runs() {
    // Full gauge, healthy: the limit goes to the first living enemy.
    let (_sim, c) = limit_battle(Scenario { limit: Some(Scenario::braver()), ..Scenario::new() }, &[ME], &[255], true);
    assert_eq!(command(&c, 1.0), Some(Command::Limit { target: Target::One(THE_DUMMY) }));
    // Not full: a plain attack.
    let (_sim, c) = limit_battle(Scenario { limit: Some(Scenario::braver()), ..Scenario::new() }, &[ME], &[254], true);
    assert_eq!(command(&c, 1.0), Some(Command::Attack { target: THE_DUMMY }));
    // Full gauge but hurt (40 of 200 is under 30%) and able to pay for a Cure: it heals first.
    let (_sim, c) = limit_battle(Scenario { limit: Some(Scenario::braver()), hp: 40, ..Scenario::new() }, &[ME], &[255], true);
    assert_eq!(command(&c, 1.0), Some(Command::Magic { spell: 0, target: Target::One(ME_AT_0) }));
    // Hurt and out of MP: no heal to give, so the limit.
    let (_sim, c) = limit_battle(Scenario { limit: Some(Scenario::braver()), hp: 40, mp: 0, ..Scenario::new() }, &[ME], &[255], true);
    assert_eq!(command(&c, 1.0), Some(Command::Limit { target: Target::One(THE_DUMMY) }));

    // A healing limit is not wasted on a party that is fine ...
    let (_sim, c) = limit_battle(Scenario { limit: Some(Scenario::healing_wind()), ..Scenario::new() }, &[ME, PEER], &[255, 0], true);
    assert_eq!(command(&c, 1.0), Some(Command::Attack { target: ActorId(2) }));
    // ... but goes to the one under half of its HP (here ourselves, at 90 of 200: not under 30%, so no spell).
    let (_sim, c) = limit_battle(Scenario { limit: Some(Scenario::healing_wind()), hp: 90, ..Scenario::new() }, &[ME, PEER], &[255, 0], true);
    assert_eq!(command(&c, 1.0), Some(Command::Limit { target: Target::AllAllies }), "Healing Wind hits the whole party at once");

    // Whatever happens, a bot with a limit never runs: play a whole battle with it.
    let (mut sim, mut c) = limit_battle(Scenario { limit: Some(Scenario::braver()), enemy_hp: 400, ..Scenario::new() }, &[ME], &[255], true);
    let mut t = 1.0;
    let mut limits = 0;
    for _ in 0..500 {
        t += 0.01;
        sim.until_our_turn(&mut c, &mut t);
        if sim.state.outcome().is_some() {
            break;
        }
        let next = c.bot_command(t).expect("our turn");
        match next {
            BattleCmd::Command { command: Command::Escape, .. } => panic!("the bot ran"),
            BattleCmd::Command { command: Command::Limit { .. }, .. } => limits += 1,
            _ => {}
        }
        send(&mut sim, &mut c, next, t);
    }
    assert_eq!(sim.state.outcome(), Some(Outcome::Victory));
    assert!(limits >= 1, "it gave its starting limit");
    no_divergence(&mut c, "the whole battle");
}

// ----- whole sides (protocol v8) ---------------------------------------------------------------------------

/// Fire on every enemy chosen with the keys: the pointer walks to ALL, the command reaches the server and its replay
/// (every event of both targets is checked), the numbers float over both enemies, and the All use the engine took
/// is gone from the menu (no ALL place any more, the list does not mention it).
#[test]
fn a_spell_on_every_enemy_chosen_with_the_keys_costs_the_all_use_and_the_replay_takes_it() {
    let rules: Arc<dyn BattleRules> = Arc::new(SyntheticRules::default());
    // Formation 1: two weak enemies (actors 1 and 2) against our one character.
    let mut sim = Sim::new(&*rules, 1, &[ME], true);
    let mut c = client(rules);
    c.set_names(Some(names()));
    c.on_message(0.0, sim.start());
    let me = ActorId(0);
    let mut t = 1.0;
    // The model says what the engine says: the spells of the Cure and Fire kind can hit a whole side, once.
    assert_eq!(c.menu_model_mut().all_left, 1);
    assert!(c.menu_model_mut().spells.iter().all(|s| s.reach == Reach::OneOrAll));

    let row = row_of_spell(&mut c, 27);
    assert_eq!(confirm_command(&mut c, MAGIC, t), MenuReply::Handled);
    assert_eq!(menu(&c, t).list.expect("the Magic list").all, Some(1), "the list says how many whole-side casts are left");
    assert_eq!(confirm_row(&mut c, row, t), MenuReply::Handled);
    assert_eq!(menu(&c, t).aim.expect("the pointer").one(), Some(ActorId(1)), "the pointer starts on the first enemy");
    c.menu_key(MenuKey::Right, t);
    assert_eq!(menu(&c, t).aim.expect("the pointer").one(), Some(ActorId(2)));
    c.menu_key(MenuKey::Right, t);
    assert_eq!(menu(&c, t).aim.expect("the pointer").at, AimAt::All, "one step past the last enemy is ALL");
    let MenuReply::Send(fire) = c.menu_key(MenuKey::Confirm, t) else { panic!("a command") };
    assert_eq!(fire, BattleCmd::Command { battle_id: BattleId(1), actor: me, command: Command::Magic { spell: 27, target: Target::AllEnemies } });
    let msg = send(&mut sim, &mut c, fire, t);
    let n = notes(&mut c);
    assert!(c.divergences() == 0 && !n.contains("DIVERGENCE") && !c.active().expect("a battle").diverged(), "{n}");
    let hit: Vec<ActorId> = events_of(&msg).iter().filter_map(|e| if let BattleEvent::Damage { target, .. } = e { Some(*target) } else { None }).collect();
    assert_eq!(hit, [ActorId(1), ActorId(2)], "every enemy, in order, from one announcement");
    assert_eq!(events_of(&msg).iter().filter(|e| matches!(e, BattleEvent::Action { .. })).count(), 1);
    let numbers: Vec<u8> = popups_over(&c, t).iter().map(|(who, _)| *who).collect();
    assert!(numbers.contains(&1) && numbers.contains(&2), "a number over each enemy: {numbers:?}");
    assert_eq!(banner(&c, t).as_deref(), Some("Fire"));

    // The use is spent: the model follows the replay, Fire has no ALL place any more and the pointer only walks
    // the two enemies (the engine would refuse the whole side now).
    t += 2.0;
    sim.until_our_turn(&mut c, &mut t);
    t += 2.0;
    assert_eq!(c.menu_model_mut().all_left, 0);
    assert!(c.menu_model_mut().spells.iter().all(|s| s.reach == Reach::One));
    assert!(sim.state.check_command(me, &Command::Magic { spell: 27, target: Target::AllEnemies }).is_err());
    assert_eq!(confirm_command(&mut c, MAGIC, t), MenuReply::Handled);
    assert_eq!(menu(&c, t).list.expect("the Magic list").all, None);
    assert_eq!(confirm_row(&mut c, row, t), MenuReply::Handled);
    let mut seen = Vec::new();
    for _ in 0..3 {
        seen.push(menu(&c, t).aim.expect("the pointer").at);
        c.menu_key(MenuKey::Right, t);
    }
    assert_eq!(seen, [AimAt::One(ActorId(1)), AimAt::One(ActorId(2)), AimAt::One(ActorId(1))]);
}

/// Cure on the whole party (two members): one Cure, the heal on each, and Healing Wind and Megalixir only ever
/// aim at the whole party (the pointer starts on ALL and the item needs no All use).
#[test]
fn cure_on_the_whole_party_and_a_megalixir_reach_every_member() {
    let rules: Arc<dyn BattleRules> = Arc::new(Scenario { hp: 100, all: 1, ..Scenario::new() });
    let mut sim = Sim::new(&*rules, 0, &[ME, PEER], true);
    let mut c = client(rules);
    c.on_inventory(0.0, vec![InventoryEntry::new(0, 3), InventoryEntry::new(6, 2)]);
    c.on_message(0.0, sim.start());
    let me = ActorId(0);
    let mut t = 1.0;
    let row = row_of_spell(&mut c, 0);
    assert_eq!(confirm_command(&mut c, MAGIC, t), MenuReply::Handled);
    assert_eq!(confirm_row(&mut c, row, t), MenuReply::Handled);
    // Two members: ourselves, then the friend, then ALL.
    c.menu_key(MenuKey::Left, t);
    let aim = menu(&c, t).aim.expect("the pointer");
    assert_eq!((aim.at, aim.enemy, aim.revive), (AimAt::All, false, false));
    let MenuReply::Send(cure) = c.menu_key(MenuKey::Confirm, t) else { panic!("a command") };
    assert_eq!(cure, BattleCmd::Command { battle_id: BattleId(1), actor: me, command: Command::Magic { spell: 0, target: Target::AllAllies } });
    let msg = send(&mut sim, &mut c, cure, t);
    assert!(c.divergences() == 0, "{}", notes(&mut c));
    let healed: Vec<ActorId> = events_of(&msg).iter().filter_map(|e| if let BattleEvent::Heal { target, .. } = e { Some(*target) } else { None }).collect();
    assert_eq!(healed, [ActorId(0), ActorId(1)]);

    // Megalixir: the item is only ever the whole party, so its pointer starts on ALL and the arrows stay.
    t += 2.0;
    sim.until_our_turn(&mut c, &mut t);
    t += 2.0;
    let row = row_of_item(&mut c, 6);
    assert_eq!(c.menu_model_mut().items[row].reach, Reach::All);
    assert_eq!(confirm_command(&mut c, ITEM, t), MenuReply::Handled);
    assert_eq!(confirm_row(&mut c, row, t), MenuReply::Handled);
    for key in [MenuKey::Right, MenuKey::Left] {
        c.menu_key(key, t);
        assert_eq!(menu(&c, t).aim.expect("the pointer").at, AimAt::All);
    }
    let MenuReply::Send(megalixir) = c.menu_key(MenuKey::Confirm, t) else { panic!("a command") };
    assert_eq!(megalixir, BattleCmd::Command { battle_id: BattleId(1), actor: me, command: Command::Item { item: 6, target: Target::AllAllies } });
    let msg = send(&mut sim, &mut c, megalixir, t);
    assert!(c.divergences() == 0, "{}", notes(&mut c));
    let healed: Vec<ActorId> = events_of(&msg).iter().filter_map(|e| if let BattleEvent::Heal { target, .. } = e { Some(*target) } else { None }).collect();
    assert_eq!(healed, [ActorId(0), ActorId(1)]);
}

// ----- the script (`--battle-script`) --------------------------------------------------------------------

use super::script::{BattleScript, Next, Scripted};
use super::ScriptStep::{Attack, Defend, Escape, Item, ItemAll, Limit, Magic, MagicAll};

/// A preemptive battle for the script tests (everybody's gauge is full at once) for `accounts` (member `i` is
/// `ActorId(i)`, the dummy follows them), starting with the limit gauges `limits`, whose account carries a
/// Potion x3, a Phoenix Down and a Grenade (the server's `Inventory` message).
fn script_battle(scenario: Scenario, accounts: &[AccountId], limits: &[u16]) -> (Sim, BattleClient) {
    let rules: Arc<dyn BattleRules> = Arc::new(scenario);
    let sim = Sim::with_limits(&*rules, 0, accounts, true, limits);
    let mut c = client(rules);
    c.on_message(0.0, sim.start());
    c.on_inventory(0.0, vec![InventoryEntry::new(0, 3), InventoryEntry::new(7, 1), InventoryEntry::new(9, 1)]);
    (sim, c)
}

/// The engine command a script entry comes to (it must be sendable).
fn sends(c: &BattleClient, t: f64, step: ScriptStep) -> Command {
    match c.scripted_command(t, step) {
        Scripted::Send(BattleCmd::Command { command, .. }) => command,
        other => panic!("{step}: {other:?}"),
    }
}

/// Why a script entry is skipped.
fn skips(c: &BattleClient, t: f64, step: ScriptStep) -> String {
    match c.scripted_command(t, step) {
        Scripted::Skip(why) => why,
        other => panic!("{step}: {other:?}"),
    }
}

/// The server applies a command the client sent and tells the client at `t`.
fn applied(sim: &mut Sim, c: &mut BattleClient, cmd: BattleCmd, t: f64) {
    let BattleCmd::Command { actor, command, .. } = cmd else { panic!("not a command: {cmd:?}") };
    c.on_message(t, sim.command(actor, command));
}

#[test]
fn a_script_entry_with_all_aims_at_the_whole_side_and_what_only_hits_a_side_needs_no_all() {
    let dummy = ActorId(2);
    let (_sim, mut c) = script_battle(Scenario { all: 1, limit: Some(Scenario::healing_wind()), ..Scenario::new() }, &[ME, PEER], &[255, 0]);
    c.on_inventory(0.0, vec![InventoryEntry::new(0, 3), InventoryEntry::new(6, 1), InventoryEntry::new(7, 1)]);
    // `:all` is the whole side (every enemy for an offensive spell, the party for a healing one); the plain
    // form is still one target.
    assert_eq!(sends(&c, 1.0, MagicAll(27)), Command::Magic { spell: 27, target: Target::AllEnemies });
    assert_eq!(sends(&c, 1.0, MagicAll(0)), Command::Magic { spell: 0, target: Target::AllAllies });
    assert_eq!(sends(&c, 1.0, Magic(27)), Command::Magic { spell: 27, target: Target::One(dummy) });
    // Megalixir hits the party and nothing else, so `item:6` is already that; a Potion cannot be aimed at a whole
    // side, which the engine says and the script reports.
    assert_eq!(sends(&c, 1.0, Item(6)), Command::Item { item: 6, target: Target::AllAllies });
    assert_eq!(sends(&c, 1.0, ItemAll(6)), Command::Item { item: 6, target: Target::AllAllies });
    assert!(skips(&c, 1.0, ItemAll(0)).contains("cannot be aimed that way"));
    // Healing Wind is the whole party.
    assert_eq!(sends(&c, 1.0, Limit), Command::Limit { target: Target::AllAllies });
    // Life on the whole party with nobody fallen: nobody to aim at.
    assert!(skips(&c, 1.0, MagicAll(7)).contains("nobody to aim at"));
    // Without an All materia use the spell on a whole side is refused for that, and one target is not.
    let (_sim, none) = script_battle(Scenario { all: 0, ..Scenario::new() }, &[ME, PEER], &[]);
    assert!(skips(&none, 1.0, MagicAll(27)).contains("no All materia use is left"));
    assert_eq!(sends(&none, 1.0, Magic(27)), Command::Magic { spell: 27, target: Target::One(dummy) });
    // The words round trip through the text of `--battle-script`.
    assert_eq!(MagicAll(27).to_string(), "magic:27:all");
    assert_eq!("item:6:all".parse::<ScriptStep>(), Ok(ItemAll(6)));
}

#[test]
fn a_script_entry_aims_offence_at_the_first_living_enemy_healing_at_the_most_hurt_member_and_revival_at_a_fallen_one() {
    let dummy = ActorId(2);
    // Everybody at full HP: healing goes to ourselves (the tie goes to us), offence to the dummy.
    let (_sim, c) = script_battle(Scenario { limit: Some(Scenario::braver()), ..Scenario::new() }, &[ME, PEER], &[255, 0]);
    assert_eq!(sends(&c, 1.0, Attack), Command::Attack { target: dummy });
    assert_eq!(sends(&c, 1.0, Limit), Command::Limit { target: Target::One(dummy) });
    assert_eq!(sends(&c, 1.0, Magic(27)), Command::Magic { spell: 27, target: Target::One(dummy) }, "Fire is offence");
    assert_eq!(sends(&c, 1.0, Item(9)), Command::Item { item: 9, target: Target::One(dummy) }, "a Grenade is offence");
    assert_eq!(sends(&c, 1.0, Magic(0)), Command::Magic { spell: 0, target: Target::One(ME_AT_0) }, "Cure on a party that is fine: ourselves");
    assert_eq!(sends(&c, 1.0, Item(0)), Command::Item { item: 0, target: Target::One(ME_AT_0) });
    assert_eq!(sends(&c, 1.0, Defend), Command::Defend);
    assert_eq!(sends(&c, 1.0, Escape), Command::Escape);

    // The ally the more hurt (the scenario hurts its first member, here PEER, actor 0; we are actor 1): the Cure,
    // the Potion and Healing Wind go to it. When we are the one at 50 of 200, they go to us.
    let (_sim, c) = script_battle(Scenario { hp: 50, limit: Some(Scenario::healing_wind()), ..Scenario::new() }, &[PEER, ME], &[0, 255]);
    let peer = ActorId(0);
    assert_eq!(sends(&c, 1.0, Magic(0)), Command::Magic { spell: 0, target: Target::One(peer) });
    assert_eq!(sends(&c, 1.0, Item(0)), Command::Item { item: 0, target: Target::One(peer) });
    assert_eq!(sends(&c, 1.0, Limit), Command::Limit { target: Target::AllAllies }, "Healing Wind hits the whole party, scripted or not");
    let (_sim, c) = script_battle(Scenario { hp: 50, ..Scenario::new() }, &[ME, PEER], &[]);
    assert_eq!(sends(&c, 1.0, Magic(0)), Command::Magic { spell: 0, target: Target::One(ME_AT_0) }, "we are the one at 50 of 200");

    // A fallen member: Phoenix Down and Life go to it; with nobody fallen they are skipped. A fallen member is never
    // "the most hurt": the Potion goes to one that stands.
    let (_sim, c) = script_battle(Scenario { ally_dead: true, ..Scenario::new() }, &[ME, PEER], &[]);
    assert_eq!(sends(&c, 1.0, Item(7)), Command::Item { item: 7, target: Target::One(ActorId(1)) });
    assert_eq!(sends(&c, 1.0, Magic(7)), Command::Magic { spell: 7, target: Target::One(ActorId(1)) });
    assert_eq!(sends(&c, 1.0, Item(0)), Command::Item { item: 0, target: Target::One(ME_AT_0) });
    let (_sim, c) = script_battle(Scenario::new(), &[ME, PEER], &[]);
    assert!(skips(&c, 1.0, Item(7)).contains("nobody has fallen"));
    assert!(skips(&c, 1.0, Magic(7)).contains("nobody has fallen"));
}

#[test]
fn what_the_engine_or_the_bag_refuses_is_skipped_with_its_reason_and_it_is_only_our_turn_when_the_bot_says_so() {
    let one = |scenario: Scenario, limits: &[u16]| script_battle(scenario, &[ME], limits).1;
    // MP and spells.
    let c = one(Scenario { mp: 0, ..Scenario::new() }, &[]);
    assert!(skips(&c, 1.0, Magic(0)).contains("not enough MP"));
    assert!(skips(&c, 1.0, Magic(99)).contains("does not know spell 99"));
    let c = one(Scenario { mp: 4, ..Scenario::new() }, &[]);
    assert!(skips(&c, 1.0, Magic(0)).contains("not enough MP"), "Cure costs 5");
    assert_eq!(sends(&c, 1.0, Magic(27)), Command::Magic { spell: 27, target: Target::One(THE_DUMMY) }, "Fire costs 4");
    // The bag: none left (the inventory the server told, zero stacks are no stacks) and not an item of the game at all.
    let (_sim, mut c) = script_battle(Scenario::new(), &[ME], &[]);
    c.on_inventory(0.5, vec![InventoryEntry::new(0, 0), InventoryEntry::new(9, 1)]);
    assert!(skips(&c, 1.0, Item(0)).contains("none of item 0 is left"));
    assert!(skips(&c, 1.0, Item(7)).contains("none of item 7 is left"), "a Phoenix Down it does not carry");
    assert!(skips(&c, 1.0, Item(42)).contains("item 42 cannot be used in battle"));
    assert_eq!(sends(&c, 1.0, Item(9)), Command::Item { item: 9, target: Target::One(THE_DUMMY) });
    // The limit: not full, and none at all.
    let c = one(Scenario { limit: Some(Scenario::braver()), ..Scenario::new() }, &[254]);
    assert!(skips(&c, 1.0, Limit).contains("the limit gauge is not full"));
    let c = one(Scenario::new(), &[255]);
    assert!(skips(&c, 1.0, Limit).contains("no limit break"));
    // A battle that cannot be escaped.
    let c = one(Scenario { escape: None, ..Scenario::new() }, &[]);
    assert!(skips(&c, 1.0, Escape).contains("cannot be escaped"));
    assert_eq!(sends(&c, 1.0, Defend), Command::Defend, "whatever else cannot be done, Defend can");

    // Not our turn: our gauge is not full, a command of ours is in flight, there is no battle, no replay.
    let rules: Arc<dyn BattleRules> = Arc::new(Scenario::new());
    let slow = Sim::new(&*rules, 0, &[ME], false);
    let mut c = client(rules);
    c.on_message(0.0, slow.start());
    assert_eq!(c.scripted_command(1.0, Defend), Scripted::NotNow, "our gauge is not full");
    let (_sim, mut c) = script_battle(Scenario::new(), &[ME], &[]);
    let cmd = BattleCmd::Command { battle_id: BattleId(1), actor: ME_AT_0, command: Command::Defend };
    c.note_sent(&cmd, 1.0);
    assert_eq!(c.scripted_command(1.5, Defend), Scripted::NotNow, "in flight");
    assert!(matches!(c.scripted_command(1.0 + PENDING_TIMEOUT_S, Defend), Scripted::Send(_)), "a lost command may be sent again");
    assert_eq!(BattleClient::new(ME).scripted_command(1.0, Defend), Scripted::NotNow, "no battle");
    let mut blind = BattleClient::new(ME);
    blind.set_rules(None);
    blind.on_message(0.0, script_battle(Scenario::new(), &[ME], &[]).0.start());
    assert_eq!(blind.scripted_command(1.0, Defend), Scripted::NotNow, "no battle data to replay with");
}

#[test]
fn a_refused_entry_is_skipped_for_good_and_the_next_one_is_tried_in_the_same_turn() {
    // No MP for Cure and a limit gauge of 254: both are skipped at once and Defend goes out in the same call.
    let (mut sim, mut c) = script_battle(Scenario { mp: 0, limit: Some(Scenario::braver()), ..Scenario::new() }, &[ME], &[254]);
    let mut script = BattleScript::new(vec![Magic(0), Limit, Defend, Escape]);
    let mut t = 1.0;
    let Next::Send(cmd) = script.next_command(t, &c) else { panic!("{:?}", script.drain_notes()) };
    assert_eq!(cmd, BattleCmd::Command { battle_id: BattleId(1), actor: ME_AT_0, command: Command::Defend });
    let notes = script.drain_notes().join("\n");
    assert!(notes.contains("entry 1 of 4 (magic:0) skipped: not enough MP"), "{notes}");
    assert!(notes.contains("entry 2 of 4 (limit) skipped: the limit gauge is not full"), "{notes}");
    assert!(notes.contains("entry 3 of 4 (defend): sent"), "{notes}");
    assert_eq!((script.stats().skipped, script.stats().applied), (2, 0));

    // Until the server answers the script waits; once Defend is applied it is used up and our gauge is spent.
    c.note_sent(&cmd, t);
    assert_eq!(script.next_command(t + 0.1, &c), Next::Wait);
    applied(&mut sim, &mut c, cmd, t + 0.1);
    assert_eq!(script.next_command(t + 0.2, &c), Next::Wait, "our gauge is spent");
    assert_eq!(script.stats().applied, 1);
    // The next turn gives the last entry, Escape (the skipped ones are never tried again), and then it is used up.
    t += 1.0;
    sim.until_our_turn(&mut c, &mut t);
    let Next::Send(cmd) = script.next_command(t, &c) else { panic!("{:?}", script.drain_notes()) };
    assert!(matches!(cmd, BattleCmd::Command { command: Command::Escape, .. }), "{cmd:?}");
    c.note_sent(&cmd, t);
    applied(&mut sim, &mut c, cmd, t);
    assert_eq!(sim.state.outcome(), Some(Outcome::Escaped), "a sure escape");
    c.on_message(t + 0.1, sim.end(&[]));
    assert_eq!(script.next_command(t + 0.2, &c), Next::Done);
    let s = script.stats();
    assert_eq!((s.entries, s.applied, s.skipped, s.refused, s.lost, s.unused), (4, 2, 2, 0, 0, 0));
    assert_eq!(script.summary(), "entries=4 applied=2 skipped=2 refused=0 lost=0 unused=0");
    no_divergence(&mut c, "the script's battle");
}

#[test]
fn an_entry_that_got_no_answer_is_sent_again_as_the_same_entry_and_a_late_refusal_of_the_duplicate_skips_nothing() {
    let (mut sim, mut c) = script_battle(Scenario::new(), &[ME], &[]);
    let mut script = BattleScript::new(vec![Defend, Magic(0), Escape]);
    let t = 1.0;
    let Next::Send(first) = script.next_command(t, &c) else { panic!() };
    c.note_sent(&first, t);
    // No answer yet: the script waits, the whole timeout through.
    assert_eq!(script.next_command(t + 0.5, &c), Next::Wait);
    assert_eq!(script.next_command(t + PENDING_TIMEOUT_S - 0.01, &c), Next::Wait);
    // Then the same command goes again (the SAME entry, not the next one) and is noted as a resend.
    let Next::Send(again) = script.next_command(t + PENDING_TIMEOUT_S, &c) else { panic!() };
    assert_eq!(again, first, "the same entry, not the next one");
    c.note_sent(&again, t + PENDING_TIMEOUT_S);
    let notes = script.drain_notes().join("\n");
    assert!(notes.contains("entry 1 of 3 (defend): sent") && notes.contains("entry 1 of 3 (defend): sent again"), "{notes}");
    assert_eq!(script.stats().skipped, 0);

    // The first copy was applied after all (a slow server); the duplicate is refused right behind it.
    let late = t + PENDING_TIMEOUT_S + 0.05;
    applied(&mut sim, &mut c, first, late);
    c.on_message(late + 0.01, ServerMsg::BattleRejected { battle_id: Some(BattleId(1)), reason: BattleRejection::GaugeNotFull });
    assert_eq!(script.next_command(late + 0.02, &c), Next::Wait, "our gauge is spent");
    let s = script.stats();
    assert_eq!((s.applied, s.refused, s.skipped, s.unused), (1, 0, 0, 2), "the entry is used up once, and the duplicate's refusal skipped nothing");

    // The next turn gives the NEXT entry, which the server applies normally.
    let mut t = late + 1.0;
    sim.until_our_turn(&mut c, &mut t);
    let Next::Send(second) = script.next_command(t, &c) else { panic!("{:?}", script.drain_notes()) };
    assert!(matches!(second, BattleCmd::Command { command: Command::Magic { spell: 0, .. }, .. }), "{second:?}");
    c.note_sent(&second, t);
    applied(&mut sim, &mut c, second, t);
    assert_eq!(script.next_command(t + 0.1, &c), Next::Wait);
    let s = script.stats();
    assert_eq!((s.applied, s.refused, s.lost, s.unused), (2, 0, 0, 1));

    // Both the answer and the duplicate's refusal between two looks: the entry still counts as applied.
    let (mut sim, mut c) = script_battle(Scenario::new(), &[ME], &[]);
    let mut script = BattleScript::new(vec![Defend, Escape]);
    let Next::Send(first) = script.next_command(1.0, &c) else { panic!() };
    c.note_sent(&first, 1.0);
    let Next::Send(_) = script.next_command(1.0 + PENDING_TIMEOUT_S, &c) else { panic!() };
    applied(&mut sim, &mut c, first, 2.05);
    c.on_message(2.06, ServerMsg::BattleRejected { battle_id: Some(BattleId(1)), reason: BattleRejection::GaugeNotFull });
    assert_eq!(script.next_command(2.1, &c), Next::Wait);
    let s = script.stats();
    assert_eq!((s.applied, s.refused, s.unused), (1, 0, 1), "applied wins over the refusal that came with it");
}

#[test]
fn an_entry_the_server_refuses_is_used_up_and_counted_and_the_script_goes_on() {
    let (mut sim, mut c) = script_battle(Scenario::new(), &[ME], &[]);
    let mut script = BattleScript::new(vec![Magic(0), Defend]);
    let Next::Send(cmd) = script.next_command(1.0, &c) else { panic!() };
    c.note_sent(&cmd, 1.0);
    // The server says no (say its MP ran out first): the entry is gone, not retried, and it is noted why.
    c.on_message(1.1, ServerMsg::BattleRejected { battle_id: Some(BattleId(1)), reason: BattleRejection::NotEnoughMp });
    let Next::Send(next) = script.next_command(1.2, &c) else { panic!("{:?}", script.drain_notes()) };
    assert!(matches!(next, BattleCmd::Command { command: Command::Defend, .. }), "{next:?}");
    let s = script.stats();
    assert_eq!((s.refused, s.skipped, s.applied, s.unused), (1, 0, 0, 1));
    let notes = script.drain_notes().join("\n");
    assert!(notes.contains("entry 1 of 2 (magic:0) was refused by the server") && notes.contains("MP"), "{notes}");
    c.note_sent(&next, 1.2);
    applied(&mut sim, &mut c, next, 1.3);
    assert_eq!(script.next_command(1.4, &c), Next::Done);
    assert_eq!(script.summary(), "entries=2 applied=1 skipped=0 refused=1 lost=0 unused=0");
}

#[test]
fn a_battle_that_ends_before_the_server_answered_resolves_the_entry_and_the_script_goes_on_in_the_next_battle() {
    let (_sim, mut c) = script_battle(Scenario::new(), &[ME], &[]);
    let mut script = BattleScript::new(vec![Defend, Defend]);
    let Next::Send(cmd) = script.next_command(1.0, &c) else { panic!() };
    c.note_sent(&cmd, 1.0);
    // The battle ends (the server's word) with no answer to the command: it is lost, never left in flight.
    c.on_message(1.1, ServerMsg::BattleEnd { battle_id: BattleId(1), outcome: Outcome::Escaped, rewards: vec![] });
    assert!(!c.in_battle());
    script.settle(&c);
    let s = script.stats();
    assert_eq!((s.lost, s.applied, s.refused, s.unused), (1, 0, 0, 1));
    assert!(script.drain_notes().join("\n").contains("got no answer before the battle ended"));

    // A second battle: the second entry is the next one.
    let rules: Arc<dyn BattleRules> = Arc::new(Scenario::new());
    let mut sim2 = Sim::new(&*rules, 0, &[ME], true);
    sim2.id = BattleId(2);
    let mut c2 = client(rules);
    c2.on_message(0.0, sim2.start());
    let Next::Send(cmd) = script.next_command(2.0, &c2) else { panic!("{:?}", script.drain_notes()) };
    assert_eq!(cmd, BattleCmd::Command { battle_id: BattleId(2), actor: ME_AT_0, command: Command::Defend });
    c2.note_sent(&cmd, 2.0);
    applied(&mut sim2, &mut c2, cmd, 2.1);
    assert_eq!(script.next_command(2.2, &c2), Next::Done);
    assert_eq!(script.summary(), "entries=2 applied=1 skipped=0 refused=0 lost=1 unused=0");

    // An applied command that ended the battle (a limit that wins it) counts as applied, not lost.
    let (mut sim, mut c) = script_battle(Scenario { limit: Some(Scenario::braver()), enemy_hp: 100, ..Scenario::new() }, &[ME], &[255]);
    let mut script = BattleScript::new(vec![Limit]);
    let Next::Send(cmd) = script.next_command(1.0, &c) else { panic!() };
    c.note_sent(&cmd, 1.0);
    applied(&mut sim, &mut c, cmd, 1.1);
    c.on_message(1.2, sim.end(&[]));
    assert!(!c.in_battle());
    assert_eq!(script.next_command(1.3, &c), Next::Done);
    let s = script.stats();
    assert_eq!((s.applied, s.lost, s.unused), (1, 0, 0), "{}", script.summary());
}

#[test]
fn the_headless_bot_gives_its_script_in_order_then_plays_as_usual() {
    use crate::party::PartyMirror;
    // Braver with a full gauge, Cure and a Potion in the bag; a tough dummy that keeps the battle going.
    let (mut sim, mut c) = script_battle(Scenario { limit: Some(Scenario::braver()), enemy_hp: 5_000, ..Scenario::new() }, &[ME], &[255]);
    let mut bot = BattleBot::new(true, false, 1).with_script(vec![Defend, Magic(0), Item(0), Limit]);
    assert_eq!(bot.script_steps().len(), 4);
    let party = PartyMirror::new(ME);
    let mut t = 1.0;
    let mut sent = Vec::new();
    for _ in 0..7 {
        let cmds = bot.step(t, &c, &party);
        assert!(cmds.len() <= 1);
        let Some(cmd) = cmds.into_iter().next() else {
            sim.until_our_turn(&mut c, &mut t);
            continue;
        };
        let BattleCmd::Command { command, .. } = cmd else { panic!("{cmd:?}") };
        sent.push(command);
        send(&mut sim, &mut c, cmd, t);
        t += 0.5;
        sim.until_our_turn(&mut c, &mut t);
    }
    let me = ME_AT_0;
    assert_eq!(
        sent[..4],
        [Command::Defend, Command::Magic { spell: 0, target: Target::One(me) }, Command::Item { item: 0, target: Target::One(me) }, Command::Limit { target: Target::One(THE_DUMMY) }],
        "{sent:?}"
    );
    assert!(sent[4..].iter().all(|c| matches!(c, Command::Attack { .. })), "then the usual rule: the limit gauge is empty, so it attacks: {sent:?}");
    assert!(sent.len() >= 6, "{sent:?}");
    assert_eq!(bot.script_summary(&c).as_deref(), Some("entries=4 applied=4 skipped=0 refused=0 lost=0 unused=0"));
    let notes = bot.drain_script_notes().join("\n");
    assert!(notes.contains("entry 4 of 4 (limit): sent, aimed at actor 1"), "{notes}");
    no_divergence(&mut c, "the scripted bot");

    // Without a script, or with an empty one, nothing changes: no steps, no summary.
    let mut plain = BattleBot::new(true, false, 1).with_script(Vec::new());
    assert!(plain.script_steps().is_empty() && plain.script_summary(&c).is_none() && plain.drain_script_notes().is_empty());
}
