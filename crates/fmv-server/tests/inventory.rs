//! Items over real TCP sockets (protocol v6, milestone M2-e): the inventory a player is told when it
//! joins, items used in battle (spent by the server only when the engine accepts the command), the items
//! a won battle drops, and all of it kept in the store across a server restart, including for an account
//! whose record was written by the previous version (no inventory yet).
//!
//! The area is the FIXTURE plaza and the rules are `fmv_protocol::SyntheticRules` (every fighter knows
//! Cure, Life and Fire; the battle accepts Potion 0, Ether 3, Phoenix Down 7 and Grenade 20; a new
//! account starts with 3 Potions, 1 Ether and 1 Phoenix Down). These tests cover the SERVER's inventory
//! machinery, not FF7 data. The one exception is the last test, which is `#[ignore]`d and plays the same
//! commands against the owner's real data (`FMV_FF7_LIBRARY`); the real data over real processes is
//! `scripts/battle-smoke.sh --progress`.
//!
//! Every client here replays the battle exactly as the real client does (`BattleReplay`), so a passing
//! test is also a proof that the messages carrying spells, items and guards are ones a replay accepts.

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use common::*;
use fmv_ff7_battle::rules::{BattleState, Command, Outcome};
use fmv_protocol::{
    ActorId, BattleRejection, BattleReplay, BattleReward, BattleRules, BattleSetup, ClientMsg, InventoryEntry, PartyEvent, ServerMsg,
    SyntheticRules, Target,
};
use fmv_server::ff7::{open_battle_data, Ff7Source};
use fmv_server::{BattleSettings, ServerConfig};
use tempfile::TempDir;

/// Fast battles (speed 0, 1000 engine ticks a second) on a server that starts them on request, with
/// `rules`, keeping its accounts in `state_dir` (or in a private temporary one).
fn fast(rules: SyntheticRules, state_dir: Option<PathBuf>) -> ServerConfig {
    fast_with(Arc::new(rules), state_dir)
}

/// [`fast`] with any rules (the real-data test passes the owner's own).
fn fast_with(rules: Arc<dyn BattleRules>, state_dir: Option<PathBuf>) -> ServerConfig {
    ServerConfig {
        battle: Some(BattleSettings { battle_speed: 0, ticks_per_second: 1000, ..BattleSettings::new(rules) }),
        debug_encounters: true,
        state_dir,
        ..ServerConfig::default()
    }
}

/// An inventory as `(item, count)` pairs.
type Items = Vec<(u16, u16)>;

fn items_of(m: &ServerMsg) -> Option<Items> {
    match m {
        ServerMsg::Inventory { items } => Some(items.iter().map(|e| (e.item, e.count)).collect()),
        _ => None,
    }
}

async fn next_inventory(j: &mut Joined) -> Items {
    j.client.until("Inventory", items_of).await
}

/// The first `Inventory` of a login (after the progress that follows the handshake).
async fn joined_with(j: &mut Joined) -> Items {
    j.client.until("CharacterProgress", |m| matches!(m, ServerMsg::CharacterProgress { .. }).then_some(())).await;
    next_inventory(j).await
}

/// What a player saw of one battle.
#[derive(Debug)]
struct Fought {
    outcome: Outcome,
    rewards: Vec<BattleReward>,
    /// The inventories the server sent while the battle ran, in order.
    inventories: Vec<Items>,
    /// What the server refused, in order.
    refused: Vec<BattleRejection>,
    /// The commands of ours the server applied, in order.
    applied: Vec<Command>,
}

/// Play one battle as the real client does: replay every message, give the command `choose` picks
/// whenever our gauge is full (one at a time; `turn` counts our commands), and check the end.
async fn fight_with(
    j: &mut Joined,
    rules: &dyn BattleRules,
    mut choose: impl FnMut(&BattleState, ActorId, u32) -> Command,
) -> Fought {
    let start = j.client.until("EncounterStart", |m| matches!(m, ServerMsg::EncounterStart { .. }).then(|| m.clone())).await;
    let ServerMsg::EncounterStart { battle_id, formation_id, seed, party, preemptive, back_attack, battle_speed, ticks_per_second, .. } = start
    else {
        unreachable!()
    };
    let setup = BattleSetup { formation_id, seed, party: party.clone(), preemptive, back_attack, battle_speed, ticks_per_second };
    let mut replay = BattleReplay::start(rules, &setup).expect("the client builds the battle from its own data");
    let mine = ActorId(party.iter().position(|m| m.entity == j.entity).expect("we are in the party") as u8);
    let (mut pending, mut turn) = (false, 0u32);
    let (mut inventories, mut refused, mut applied) = (Vec::new(), Vec::new(), Vec::new());
    loop {
        let state = replay.state();
        if !pending && state.outcome().is_none() && state.ready_actors().contains(&mine) {
            let command = choose(state, mine, turn);
            turn += 1;
            j.client.send(&ClientMsg::BattleCommand { battle_id, actor: mine, command }).await;
            pending = true;
        }
        match j.client.next().await {
            ServerMsg::BattleEvent { tick, events, command, .. } => {
                replay.apply(tick, &events, command.as_ref()).unwrap_or_else(|d| panic!("the replay diverged: {d}"));
                if let Some(c) = command.filter(|c| c.actor == mine) {
                    pending = false;
                    applied.push(c.command);
                }
            }
            ServerMsg::BattleRejected { reason, .. } => {
                pending = false;
                refused.push(reason);
            }
            ServerMsg::Inventory { items } => inventories.push(items.iter().map(|e| (e.item, e.count)).collect()),
            ServerMsg::BattleEnd { outcome, rewards, .. } => {
                replay.check_end(outcome, &rewards).unwrap_or_else(|d| panic!("the end diverged: {d}"));
                return Fought { outcome, rewards, inventories, refused, applied };
            }
            _ => {}
        }
    }
}

fn first_enemy(state: &BattleState) -> ActorId {
    state.enemy_ids().into_iter().find(|e| state.actor(*e).is_some_and(|c| c.is_alive())).expect("a living enemy")
}

/// Attack whenever ready.
fn attack(state: &BattleState, _me: ActorId, _turn: u32) -> Command {
    Command::Attack { target: first_enemy(state) }
}

async fn request(j: &mut Joined, formation: u16) {
    j.client.send(&ClientMsg::RequestEncounter { formation_id: Some(formation) }).await;
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

const STOCK: [(u16, u16); 3] = [(0, 3), (3, 1), (7, 1)];

// ---------------------------------------------------------------------------------------------

/// The inventory is the fourth message of a join: the snapshot and the placement stay first, the
/// progress is third. A server without battle data has no items and sends none.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_player_is_told_what_it_carries_right_after_where_its_character_stands() {
    let server = TestServer::start(fast(SyntheticRules::default(), None)).await;
    let mut a = join(server.addr, &uniq("Iv")).await;
    let first = a.client.next().await;
    assert!(matches!(first, ServerMsg::CharacterProgress { .. }), "the third message after the handshake: {first:?}");
    let second = a.client.next().await;
    assert_eq!(items_of(&second), Some(STOCK.to_vec()), "a new account starts with the game's starting stock: {second:?}");
    server.shutdown().await;

    let plain = TestServer::start(ServerConfig::default()).await;
    let mut b = join(plain.addr, &uniq("Fx")).await;
    let quiet = b.client.drain_for(Duration::from_millis(400)).await;
    assert!(quiet.iter().all(|m| items_of(m).is_none()), "the FIXTURE server has no items: {quiet:?}");
    plain.shutdown().await;
}

/// Every kind of command over the wire: a spell, a guard, an item and attacks, replayed by the client
/// without a divergence; the item is spent (and told) right after the event that applied it, the spell
/// and the guard cost no item.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spells_guards_and_items_replay_and_only_the_item_is_spent() {
    let rules = SyntheticRules::default();
    let server = TestServer::start(fast(rules, None)).await;
    let mut a = join(server.addr, &uniq("Mx")).await;
    assert_eq!(joined_with(&mut a).await, STOCK.to_vec());
    request(&mut a, 1).await;
    let fought = fight_with(&mut a, &rules, |state, me, turn| {
        let foe = first_enemy(state);
        match turn {
            0 => Command::Magic { spell: 27, target: Target::One(foe) },
            1 => Command::Defend,
            2 => Command::Item { item: 3, target: Target::One(me) },
            3 => Command::Magic { spell: 0, target: Target::One(me) },
            _ => Command::Attack { target: foe },
        }
    })
    .await;
    assert_eq!(fought.outcome, Outcome::Victory);
    assert!(fought.refused.is_empty(), "every command was one the engine accepts: {:?}", fought.refused);
    assert_eq!(
        &fought.applied[..4],
        [
            Command::Magic { spell: 27, target: Target::One(ActorId(1)) },
            Command::Defend,
            Command::Item { item: 3, target: Target::One(ActorId(0)) },
            Command::Magic { spell: 0, target: Target::One(ActorId(0)) },
        ]
    );
    // One Ether used: one inventory message, the Ether gone from the stack list.
    assert_eq!(fought.inventories, vec![vec![(0, 3), (7, 1)]]);
    server.shutdown().await;
}

/// What the server refuses, over the wire: the engine's reasons first, and an item the account does not
/// have, none of it costing anything, and the turn is still ours.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refused_commands_say_why_and_cost_nothing() {
    let rules = SyntheticRules::default();
    let dir = TempDir::new().unwrap();
    let state = dir.path().join("server-state");
    let server = TestServer::start(fast(rules, Some(state.clone()))).await;
    let mut a = join(server.addr, &uniq("Rf")).await;
    assert_eq!(joined_with(&mut a).await, STOCK.to_vec());
    request(&mut a, 0).await;
    let fought = fight_with(&mut a, &rules, |state, me, turn| {
        let foe = first_enemy(state);
        match turn {
            0 => Command::Item { item: 8, target: Target::One(me) },       // not usable in battle
            1 => Command::Item { item: 7, target: Target::One(me) },       // Phoenix Down on a living one
            2 => Command::Magic { spell: 99, target: Target::One(foe) },   // no such spell
            3 => Command::Magic { spell: 0, target: Target::One(foe) },    // Cure on an enemy
            4 => Command::Item { item: 20, target: Target::One(foe) },     // a Grenade: the account has none
            // Whole sides (protocol v8): the party's item aimed at one ally is no way to aim it, and the engine
            // says so before the inventory is asked; Cure on every enemy is the wrong side; Megalixir on the
            // party is a good command, which the account has no Megalixir for.
            5 => Command::Item { item: 6, target: Target::One(me) },
            6 => Command::Magic { spell: 0, target: Target::AllEnemies },
            7 => Command::Item { item: 6, target: Target::AllAllies },
            _ => Command::Attack { target: foe },
        }
    })
    .await;
    assert_eq!(
        &fought.refused[..8],
        [
            BattleRejection::UnknownItem,
            BattleRejection::TargetNotDead,
            BattleRejection::UnknownSpell,
            BattleRejection::InvalidTarget,
            BattleRejection::OutOfItems,
            BattleRejection::InvalidTarget,
            BattleRejection::InvalidTarget,
            BattleRejection::OutOfItems,
        ]
    );
    assert!(fought.inventories.is_empty(), "nothing was spent: {:?}", fought.inventories);
    assert_eq!(fought.outcome, Outcome::Victory);
    drop(a);
    server.shutdown().await;
    // Nothing item-related was written: the record has no inventory at all (it is a record of numbers).
    let file = state.join("progression");
    let record = std::fs::read_dir(&file).unwrap().next().unwrap().unwrap().path().join("Fixture.character.json");
    let text = std::fs::read_to_string(&record).unwrap();
    assert!(!text.contains("inventory"), "{text}");
}

/// The durability: use a Potion, stop the server gracefully, start another on the same state directory,
/// log in with the token, and the join message says one Potion fewer. The starting stock is not given
/// again (the Potion stays gone), and the next battle goes on from there.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_used_item_is_gone_after_a_restart_and_the_starting_stock_is_not_given_twice() {
    let rules = SyntheticRules::default();
    let dir = TempDir::new().unwrap();
    let state = dir.path().join("server-state");
    let server = TestServer::start(fast(rules, Some(state.clone()))).await;
    let mut a = join(server.addr, &uniq("Rs")).await;
    let token = a.token.clone();
    assert_eq!(joined_with(&mut a).await, STOCK.to_vec());
    request(&mut a, 0).await;
    let fought = fight_with(&mut a, &rules, |state, me, turn| {
        if turn == 0 {
            Command::Item { item: 0, target: Target::One(me) }
        } else {
            attack(state, me, turn)
        }
    })
    .await;
    assert_eq!(fought.inventories, vec![vec![(0, 2), (3, 1), (7, 1)]]);
    drop(a);
    server.shutdown().await;

    let again = TestServer::start(fast(rules, Some(state.clone()))).await;
    let mut a2 = join_with_token(again.addr, &token).await;
    assert_eq!(joined_with(&mut a2).await, vec![(0, 2), (3, 1), (7, 1)], "what was saved, not the stock again");
    // And it goes on from there: the last Phoenix Down can be used... on nobody alive. Use two Potions.
    request(&mut a2, 0).await;
    let second = fight_with(&mut a2, &rules, |state, me, turn| if turn < 2 { Command::Item { item: 0, target: Target::One(me) } } else { attack(state, me, turn) }).await;
    assert_eq!(second.inventories, vec![vec![(0, 1), (3, 1), (7, 1)], vec![(3, 1), (7, 1)]]);
    drop(a2);
    again.shutdown().await;

    let last = TestServer::start(fast(rules, Some(state))).await;
    let mut a3 = join_with_token(last.addr, &token).await;
    assert_eq!(joined_with(&mut a3).await, vec![(3, 1), (7, 1)], "the stack that was used up is gone for good");
    last.shutdown().await;
}

/// A win drops items to the members that are alive, in the reward and then in an `Inventory` after the
/// `CharacterProgress`; they are saved under each account and are there after a restart.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_win_drops_items_that_are_told_saved_and_there_after_a_restart() {
    let rules = SyntheticRules { drop_item: Some(0), ..SyntheticRules::default() };
    let dir = TempDir::new().unwrap();
    let state = dir.path().join("server-state");
    let server = TestServer::start(fast(rules, Some(state.clone()))).await;
    let mut a = join(server.addr, &uniq("Da")).await;
    let mut b = join(server.addr, &uniq("Db")).await;
    let (a_token, b_token) = (a.token.clone(), b.token.clone());
    assert_eq!(joined_with(&mut a).await, STOCK.to_vec());
    assert_eq!(joined_with(&mut b).await, STOCK.to_vec());
    party_up(&mut a, &mut b).await;

    // Two dummies, one Potion each, one to each of the two living members.
    request(&mut a, 1).await;
    let (fa, fb) = tokio::join!(fight_with(&mut a, &rules, attack), fight_with(&mut b, &rules, attack));
    assert_eq!(fa.outcome, Outcome::Victory);
    for fought in [&fa, &fb] {
        assert_eq!(fought.rewards.len(), 2);
        assert!(fought.rewards.iter().all(|r| r.items == vec![InventoryEntry::new(0, 1)]), "{:?}", fought.rewards);
    }
    // Told after the battle's end and the progress: a Potion more.
    assert_eq!(next_inventory(&mut a).await, vec![(0, 4), (3, 1), (7, 1)]);
    assert_eq!(next_inventory(&mut b).await, vec![(0, 4), (3, 1), (7, 1)]);
    drop((a, b));
    server.shutdown().await;

    let again = TestServer::start(fast(rules, Some(state))).await;
    for token in [a_token, b_token] {
        let mut j = join_with_token(again.addr, &token).await;
        assert_eq!(joined_with(&mut j).await, vec![(0, 4), (3, 1), (7, 1)]);
    }
    again.shutdown().await;
}

/// A defeat drops nothing and tells nobody anything about items.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_defeat_drops_nothing() {
    let rules = SyntheticRules { drop_item: Some(0), ..SyntheticRules::default() };
    let server = TestServer::start(fast(rules, None)).await;
    let mut a = join(server.addr, &uniq("Df")).await;
    assert_eq!(joined_with(&mut a).await, STOCK.to_vec());
    request(&mut a, 2).await; // one overwhelming enemy
    let lost = fight_with(&mut a, &rules, attack).await;
    assert_eq!(lost.outcome, Outcome::Defeat);
    assert!(lost.rewards.is_empty());
    let after = a.client.drain_for(Duration::from_millis(500)).await;
    assert!(after.iter().all(|m| items_of(m).is_none()), "{after:?}");
    server.shutdown().await;
}

/// The save compatibility the owner's preview server depends on: an account whose record was written by
/// the previous version has numbers and NO inventory. It joins with the starting stock, a battle that only
/// pays numbers leaves its record exactly as the previous version wrote it, and the first item it uses
/// writes the stock (minus that item) ONCE: it is never given the stock a second time.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_account_of_the_previous_version_gets_the_starting_stock_once() {
    let rules = SyntheticRules::default();
    let dir = TempDir::new().unwrap();
    let state = dir.path().join("server-state");
    let (account, token) = {
        let store = fmv_persist::Store::open(&state).expect("a new state directory");
        let (account, token) = store.create_account("OldTimer", 1_000).unwrap();
        // The v5 shape: no `inventory` key at all.
        let folder = state.join("progression").join(account.0.to_string());
        std::fs::create_dir_all(&folder).unwrap();
        let record = format!(
            r#"{{"schema_version":1,"account_id":{},"game":"Fixture","character":0,"level":5,"exp":55,"ap":3,"gil":9,"updated_at":1000}}"#,
            account.0
        );
        std::fs::write(folder.join("Fixture.character.json"), record).unwrap();
        (account, token)
    };
    let record_path = state.join("progression").join(account.0.to_string()).join("Fixture.character.json");

    // Join: the numbers it had, and the starting stock.
    let server = TestServer::start(fast(rules, Some(state.clone()))).await;
    let mut a = join_with_token(server.addr, &token).await;
    assert_eq!(joined_with(&mut a).await, STOCK.to_vec());
    // A won battle with no items in it pays the numbers and leaves the record without an inventory.
    request(&mut a, 0).await;
    assert_eq!(fight_with(&mut a, &rules, attack).await.outcome, Outcome::Victory);
    drop(a);
    server.shutdown().await;
    let text = std::fs::read_to_string(&record_path).unwrap();
    assert!(!text.contains("inventory"), "a payment of numbers does not invent an inventory: {text}");
    assert!(text.contains("\"exp\": 67"), "55 + 12: {text}");

    // The first item used writes the stock minus it, once.
    let server = TestServer::start(fast(rules, Some(state.clone()))).await;
    let mut a = join_with_token(server.addr, &token).await;
    assert_eq!(joined_with(&mut a).await, STOCK.to_vec(), "the same stock again: nothing was written yet");
    request(&mut a, 0).await;
    let used = fight_with(&mut a, &rules, |state, me, turn| if turn == 0 { Command::Item { item: 0, target: Target::One(me) } } else { attack(state, me, turn) }).await;
    assert_eq!(used.inventories, vec![vec![(0, 2), (3, 1), (7, 1)]]);
    drop(a);
    server.shutdown().await;
    let text = std::fs::read_to_string(&record_path).unwrap();
    assert!(text.contains("\"inventory\""), "{text}");

    let server = TestServer::start(fast(rules, Some(state))).await;
    let mut a = join_with_token(server.addr, &token).await;
    assert_eq!(joined_with(&mut a).await, vec![(0, 2), (3, 1), (7, 1)], "the stock is given once, not again");
    server.shutdown().await;
}

/// The new commands against the owner's REAL FF7 data over a real socket: the kernel's own spells, items
/// and starting stock, Defend, and the scene's enemies with their own attacks, played by a client that
/// replays the battle from the same files (so a divergence between the server's engine and the client's
/// is a failure here). `#[ignore]`d: it needs `FMV_FF7_LIBRARY`, a state directory that holds the owner's
/// imported library, which is only read (the server keeps its accounts in a private temporary directory):
///
/// ```text
/// FMV_FF7_LIBRARY=<state dir with library/> \
///   cargo test --offline -p fmv-server --test inventory -- --ignored --nocapture
/// ```
///
/// One fighter (the FIXTURE plaza gives the account Cloud) takes the debug formation 301 and gives, in
/// turn: Defend, the first stack of its stock (the Potion) on itself, a healing spell on itself, a
/// damaging spell at the first living enemy, then attacks. Passes when the replay never diverges, every
/// command is applied, and the account has lost exactly the one item it used. Only ids and counts are
/// compared; nothing of the data is printed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs the owner's library: set FMV_FF7_LIBRARY"]
async fn real_data_spells_guards_and_items_replay_without_a_divergence_and_only_the_item_is_spent() {
    let Some(dir) = std::env::var_os("FMV_FF7_LIBRARY").map(PathBuf::from) else {
        eprintln!("skipped: FMV_FF7_LIBRARY is not set");
        return;
    };
    let data = open_battle_data(&Ff7Source::Library { state_dir: Some(dir), set: None }).expect("the library has scene.bin and kernel.bin").0;
    let rules: Arc<dyn BattleRules> = data;
    let stock: Items = rules.starting_inventory().iter().map(|e| (e.item, e.count)).collect();
    let potion = stock.first().map(|&(item, _)| item).expect("the kernel's starting stock is not empty");

    let server = TestServer::start(fast_with(Arc::clone(&rules), None)).await;
    let mut a = join(server.addr, &uniq("Rl")).await;
    assert_eq!(joined_with(&mut a).await, stock, "a new account starts with the kernel's own stock");
    request(&mut a, 301).await;
    let fought = fight_with(&mut a, rules.as_ref(), |state, me, turn| {
        let foe = first_enemy(state);
        let spells = state.spells_for(me);
        let heal = spells.iter().find(|s| s.targets_allies && state.check_command(me, &Command::Magic { spell: s.spell, target: Target::One(me) }).is_ok());
        let harm = spells.iter().find(|s| !s.targets_allies);
        assert!(state.item_info(potion).is_some(), "the engine knows the item the stock starts with");
        match (turn, heal, harm) {
            (0, ..) => Command::Defend,
            (1, ..) => Command::Item { item: potion, target: Target::One(me) },
            (2, Some(s), _) => Command::Magic { spell: s.spell, target: Target::One(me) },
            (3, _, Some(s)) => Command::Magic { spell: s.spell, target: Target::One(foe) },
            _ => Command::Attack { target: foe },
        }
    })
    .await;

    assert!(fought.refused.is_empty(), "every command was one the real engine accepts: {:?}", fought.refused);
    assert!(fought.applied.len() >= 4, "the four commands before the first attack were all applied: {:?}", fought.applied);
    assert_eq!(fought.applied[0], Command::Defend);
    assert!(matches!(fought.applied[1], Command::Item { item, target: Target::One(target) } if item == potion && target == ActorId(0)), "{:?}", fought.applied[1]);
    assert!(matches!(fought.applied[2], Command::Magic { target, .. } if target == Target::One(ActorId(0))), "a healing spell on ourselves: {:?}", fought.applied[2]);
    assert!(matches!(fought.applied[3], Command::Magic { target, .. } if target != Target::One(ActorId(0))), "a damaging spell at an enemy: {:?}", fought.applied[3]);
    let mut left = stock.clone();
    left[0].1 -= 1;
    if left[0].1 == 0 {
        left.remove(0);
    }
    assert_eq!(fought.inventories, vec![left], "one item used, one inventory told, nothing else spent");
    eprintln!("real data: outcome {:?}, {} command(s) applied, {} reward(s)", fought.outcome, fought.applied.len(), fought.rewards.len());
    server.shutdown().await;
}
