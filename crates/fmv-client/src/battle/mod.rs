//! Shared battles on the client (protocol v3).
//!
//! The server owns the battle (`fmv_ff7_battle::rules::BattleState`) and announces every
//! operation on it; this client keeps a MIRROR: it rebuilds the same engine from this machine's
//! OWN `scene.bin` and `kernel.bin` (only ids, a seed and levels travel, see `fmv_protocol::battle`)
//! and replays what the server announces through [`fmv_protocol::BattleReplay`], checking that its
//! engine reproduces every announced event. A difference is a DIVERGENCE: it is counted and logged
//! loudly (a `BATTLE DIVERGENCE:` note, the window's red band, the bot's non-zero exit), never
//! papered over, because it means this client's game data or engine differs from the server's.
//!
//! * [`BattleClient`] is the mirror of the whole battle situation (no active battle, or one), fed
//!   by the four battle messages ([`BattleClient::on_message`]); it counts battles entered,
//!   victories, defeats, divergences and refusals, and says what happened as one-line notes.
//! * [`BattleView`] is what the screen shows, as plain data: the fighters ([`Bar`]), the replay's
//!   standing, and the live parts of the battle UI (the command menu of our turn, the floating
//!   numbers, the action banner, the victory or defeat panel). [`draw_battle_overlay`] draws all of
//!   it in FF7-style blue windows (`panels`): enemy list, party panel with HP/MP/ATB, our menu with
//!   its Magic and Item lists and the target pointer, the numbers over the fighters, the banner and
//!   the end panel. Without a 3D scene, [`battle_vertices`] draws the fighters as flat tokens and
//!   [`draw_battle_text`] is the overlay over them; a scene gives its own [`Anchors`] instead.
//! * [`scene::BattleScene`] is that 3D scene (a camera, a location, the enemies where the formation
//!   puts them, the party facing them, what happened played as short reactions). It is fed by
//!   [`BattleClient::scene_start`], [`BattleClient::take_events`] and [`BattleClient::engine`], and
//!   the flat view above remains the fallback.
//! * The command menu is a pure state machine (`menu`); [`BattleClient::menu_key`] feeds it the
//!   player's keys and answers with the request to send ([`MenuReply`]). Its model (spells, items,
//!   Defend) is filled by the client itself from the replayed engine (the spells our character
//!   knows, which items the engine can use) and the inventory the server sent: at every battle
//!   start and at every `Inventory` message while a battle runs (`BattleClient::refresh_menu_model`).
//!   Without a replay, or when we are not in the party, it stays empty: only Attack works and the
//!   rest is greyed out. `hud` keeps what the battle itself does not: the menu, the floating
//!   numbers, the banner (the name of each action) and the LINGER: for a few seconds after
//!   `BattleEnd` ([`BattleClient::lingering`]) [`BattleClient::view`] still returns the last view
//!   with its panel, while [`BattleClient::in_battle`] is already false (the server freed us).
//! * The inventory (protocol v6) is the server's word about what our account carries; the client
//!   keeps the last one it said ([`BattleClient::on_inventory`]), bounded whatever it says, and
//!   notes each message as one `inventory: stacks=.. total=.. items=<item>:<count>,..` line. The
//!   server sends a fresh one on joining, after each item our character used and after drops; the
//!   engine itself knows no inventory.
//! * [`BattleBot`] is the headless `--battle-auto` player: it attacks, and heals itself with the
//!   cheapest healing spell it can pay for when its HP is low ([`BattleClient::bot_command`]).
//! * [`open_rules`] reads this machine's battle data (the same library set as the field archive);
//!   [`open_names`] reads the spell, item and enemy attack names the windows show from the same files.
//!
//! While a battle runs the player is frozen in the field: loops neither walk nor send movement.
//! The mirror advances ONLY with server messages (so it can never run ahead of the server); the
//! ATB gauges in the view are extrapolated between messages for display and snap back at the next
//! one. Everything from the server is untrusted: it is bounds-checked by the replay and nothing
//! here can panic on it.

#[cfg(test)]
mod action_tests;
mod bot;
mod data;
mod hud;
mod menu;
mod names;
mod panels;
mod popups;
pub mod scene;
#[cfg(test)]
mod glue_tests;
#[cfg(test)]
mod shots;
mod text;
mod view;

pub use bot::BattleBot;
pub use data::open_rules;
pub use menu::MenuKey;
use scene::SceneStart;
pub use text::draw_battle_text;
pub use view::{battle_vertices, Bar, BattleView, Status};

// The rest of the overlay's API, for the code that wires the names (`main`), the menu's data and the
// 3D scene (the redraw): not all of it is used by this crate's own window yet.
#[allow(unused_imports)]
pub use menu::{BattleMenuModel, Level, MenuItem, MenuSpell, TargetRule};
#[allow(unused_imports)]
pub use data::open_names;
#[allow(unused_imports)]
pub use names::NameBook;
#[allow(unused_imports)]
pub use text::draw_battle_overlay;
#[allow(unused_imports)]
pub use view::{flat_anchors, Anchors};

use fmv_core::{AccountId, Fingerprint};
use fmv_ff7_battle::rules::{BattleState, Combatant, GAUGE_MAX};
use fmv_protocol::battle::{BattleEvent, Command, Outcome};
use fmv_protocol::{
    ActorId, AppliedCommand, BattleId, BattleMember, BattleRejection, BattleReplay, BattleReward, BattleRules, BattleSetup, ClientMsg,
    InventoryEntry, ServerMsg, MAX_INVENTORY_STACKS,
};
use hud::BattleHud;
use menu::{Choice, Reply};
use names::{item_name, spell_name};
use panels::{RewardItem, RewardLine};
use std::collections::VecDeque;
use std::sync::{Arc, OnceLock};

/// This machine's own battle rules, installed once per process by [`install_local_rules`]. A process
/// reads its game data once (at start-up, before any session exists) and every [`BattleClient`] it
/// creates afterwards, whichever way the session came about (command line or character menu), uses
/// the same data.
static LOCAL_RULES: OnceLock<Option<Arc<dyn BattleRules>>> = OnceLock::new();

/// Install this machine's battle rules for every [`BattleClient`] made from now on (only the first
/// call counts: the data is read once per process).
pub fn install_local_rules(rules: Option<Arc<dyn BattleRules>>) {
    let _ = LOCAL_RULES.set(rules);
}

fn local_rules() -> Option<Arc<dyn BattleRules>> {
    LOCAL_RULES.get().and_then(Clone::clone)
}

/// This machine's own spell, item and attack names for the battle windows, installed once per
/// process like the rules (see [`open_names`]). Without them the windows use plain labels.
static LOCAL_NAMES: OnceLock<Option<Arc<NameBook>>> = OnceLock::new();

/// Install this machine's names for every [`BattleClient`] made from now on (only the first call
/// counts).
pub fn install_local_names(names: Option<Arc<NameBook>>) {
    let _ = LOCAL_NAMES.set(names);
}

fn local_names() -> Option<Arc<NameBook>> {
    LOCAL_NAMES.get().and_then(Clone::clone)
}

/// A command sent for our character is considered lost (and may be sent again) after this long
/// without the server's answer.
pub const PENDING_TIMEOUT_S: f64 = 1.0;
/// Notes kept until the loop prints them; beyond this the oldest are dropped.
const MAX_NOTES: usize = 64;
/// Events kept for the scene ([`BattleClient::take_events`]); beyond this the oldest are dropped.
/// A drawn frame takes them, so this only bounds a window that stops drawing.
const MAX_EVENTS: usize = 256;
/// Divergences reported one by one; later ones are only counted.
const MAX_DIVERGENCE_NOTES: u64 = 5;

/// What the player (or the bot) asks of the server about battles. Each is sent as the matching
/// `ClientMsg`; the server decides and answers with battle messages or `BattleRejected`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BattleCmd {
    /// Command one of our characters (the server checks it is ours and ready).
    Command { battle_id: BattleId, actor: ActorId, command: Command },
    /// Debug: start a battle for our party (the server must run `--debug-encounters`).
    RequestEncounter { formation: Option<u16> },
}

impl BattleCmd {
    /// The message that carries this request.
    pub fn to_msg(self) -> ClientMsg {
        match self {
            BattleCmd::Command { battle_id, actor, command } => ClientMsg::BattleCommand { battle_id, actor, command },
            BattleCmd::RequestEncounter { formation } => ClientMsg::RequestEncounter { formation_id: formation },
        }
    }
}

/// What a key of the battle menu comes to (see [`BattleClient::menu_key`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuReply {
    /// The menu changed (the cursor moved, a level opened or closed): nothing to send.
    Handled,
    /// No menu was open for this key (not our turn), or `Back` at the top level.
    Ignored,
    /// The key was understood but cannot be done, and why.
    Refused(&'static str),
    /// Send this request.
    Send(BattleCmd),
}

/// The content of an `EncounterStart`.
#[derive(Debug, Clone)]
struct Start {
    battle_id: BattleId,
    setup: BattleSetup,
    data_fingerprint: Fingerprint,
}

/// The commands of our own character that the server applied, by kind (counted from the
/// `AppliedCommand` the server echoes in each event).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Applied {
    attack: u64,
    magic: u64,
    item: u64,
    defend: u64,
}

impl Applied {
    fn count(&mut self, command: &Command) {
        let kind = match command {
            Command::Attack { .. } => &mut self.attack,
            Command::Magic { .. } => &mut self.magic,
            Command::Item { .. } => &mut self.item,
            Command::Defend => &mut self.defend,
        };
        *kind += 1;
    }

    fn total(&self) -> u64 {
        self.attack + self.magic + self.item + self.defend
    }

    fn add(&mut self, other: Applied) {
        self.attack += other.attack;
        self.magic += other.magic;
        self.item += other.item;
        self.defend += other.defend;
    }
}

/// An inventory as this client keeps it, whatever the server sent (it is untrusted): no stack of
/// zero, an item that appears twice merged into its first place (the count saturating at `u16`),
/// at most [`MAX_INVENTORY_STACKS`] different items (the first ones), otherwise in the order sent. A
/// well-formed inventory, which the server sends (one stack per item, ascending), comes out as it went in.
fn normalise_inventory(items: Vec<InventoryEntry>) -> Vec<InventoryEntry> {
    let mut kept: Vec<InventoryEntry> = Vec::new();
    for entry in items {
        if entry.count == 0 {
            continue;
        }
        if let Some(have) = kept.iter_mut().find(|have| have.item == entry.item) {
            have.count = have.count.saturating_add(entry.count);
        } else if kept.len() < MAX_INVENTORY_STACKS {
            kept.push(entry);
        }
    }
    kept
}

/// `item:count` pairs, comma separated, for a note (`-` when there are none): ids and counts only,
/// never a name.
fn item_list(items: &[InventoryEntry]) -> String {
    if items.is_empty() {
        return "-".to_string();
    }
    items.iter().map(|e| format!("{}:{}", e.item, e.count)).collect::<Vec<_>>().join(",")
}

/// One battle this client is in.
#[derive(Debug)]
pub struct ActiveBattle {
    id: BattleId,
    /// The formation id of the setup (`scene * 4 + slot`).
    formation_id: u16,
    /// The party of the setup, `party[i]` fighting as `ActorId(i)` (kept even without a replay).
    party: Vec<BattleMember>,
    /// `None` when this client cannot rebuild the battle (no local battle data, or the data
    /// cannot build it): the battle then runs on the server without being mirrored.
    replay: Option<BattleReplay>,
    /// Our own character (`ActorId` = our index in the party), if we are in it.
    my_actor: Option<ActorId>,
    ticks_per_second: u32,
    /// Local clock (seconds) at the last server message of this battle.
    last_message: f64,
    /// When we sent a command that the server has not answered yet.
    pending: Option<f64>,
    diverged: bool,
    messages: u64,
    /// Commands of OUR character that the server applied (it echoes each one in its event), by kind.
    my_commands: Applied,
}

impl ActiveBattle {
    pub fn id(&self) -> BattleId {
        self.id
    }

    /// The replayed engine, if this client could build it.
    #[cfg(test)]
    pub fn replay(&self) -> Option<&BattleReplay> {
        self.replay.as_ref()
    }

    #[cfg(test)]
    pub fn my_actor(&self) -> Option<ActorId> {
        self.my_actor
    }

    /// Did the replay ever disagree with the server?
    #[cfg(test)]
    pub fn diverged(&self) -> bool {
        self.diverged
    }

    /// Server messages applied to this battle so far.
    #[cfg(test)]
    pub fn messages(&self) -> u64 {
        self.messages
    }
}

/// The client's mirror of its battle situation. See the module docs.
pub struct BattleClient {
    me: AccountId,
    rules: Option<Arc<dyn BattleRules>>,
    active: Option<ActiveBattle>,
    entered: u64,
    ended: u64,
    victories: u64,
    defeats: u64,
    divergences: u64,
    /// Ended battles whose whole log AND ending this machine reproduced with its own battle data.
    verified: u64,
    /// Commands of our character the server applied, over the ended battles, by kind.
    applied: Applied,
    rejections: u64,
    last_rejection: Option<BattleRejection>,
    /// What the server last said our account carries (see [`normalise_inventory`]); empty until it says.
    inventory: Vec<InventoryEntry>,
    /// `Inventory` messages received.
    inventory_updates: u64,
    notes: VecDeque<String>,
    /// What the server's messages did, oldest first, until [`BattleClient::take_events`] takes it. Kept
    /// here and not on the battle itself so that the last messages of a battle (the killing blow)
    /// can still be taken after the battle has ended.
    events: VecDeque<BattleEvent>,
    /// The live state of the battle UI: command menu, floating numbers, banner, the lingering end.
    hud: BattleHud,
}

impl std::fmt::Debug for BattleClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The rules hold this machine's game data: show only that they exist.
        f.debug_struct("BattleClient")
            .field("me", &self.me)
            .field("has_rules", &self.rules.is_some())
            .field("active", &self.active.as_ref().map(ActiveBattle::id))
            .field("entered", &self.entered)
            .field("victories", &self.victories)
            .finish_non_exhaustive()
    }
}

impl BattleClient {
    /// A client with no battle, using this machine's installed battle data (see
    /// [`install_local_rules`]; without any it cannot mirror a battle).
    pub fn new(me: AccountId) -> Self {
        Self {
            me,
            rules: local_rules(),
            active: None,
            entered: 0,
            ended: 0,
            victories: 0,
            defeats: 0,
            divergences: 0,
            verified: 0,
            applied: Applied::default(),
            rejections: 0,
            last_rejection: None,
            inventory: Vec::new(),
            inventory_updates: 0,
            notes: VecDeque::new(),
            events: VecDeque::new(),
            hud: BattleHud::new(local_names()),
        }
    }

    /// Replace the battle rules this client mirrors with (tests; a running client uses the
    /// process-wide ones).
    #[cfg(test)]
    pub fn set_rules(&mut self, rules: Option<Arc<dyn BattleRules>>) {
        self.rules = rules;
    }

    /// Replace the names the battle windows show (tests; a running client uses the process-wide ones).
    #[cfg(test)]
    pub fn set_names(&mut self, names: Option<Arc<NameBook>>) {
        self.hud.set_names(names);
    }

    /// What the command menu offers: the spells, the items, whether Defend exists. The client fills it
    /// itself (`refresh_menu_model`); tests change it to try the menu on other lists.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn menu_model_mut(&mut self) -> &mut menu::BattleMenuModel {
        self.hud.model_mut()
    }

    // ----- queries ------------------------------------------------------------------------

    /// Are we in a battle (frozen in the field)?
    pub fn in_battle(&self) -> bool {
        self.active.is_some()
    }

    #[cfg(test)]
    pub fn active(&self) -> Option<&ActiveBattle> {
        self.active.as_ref()
    }

    /// Battles we entered (`EncounterStart` received).
    pub fn entered(&self) -> u64 {
        self.entered
    }

    /// Battles that ended (`BattleEnd` received for the battle we were in).
    pub fn ended(&self) -> u64 {
        self.ended
    }

    /// Battles the server ended in victory.
    pub fn victories(&self) -> u64 {
        self.victories
    }

    #[cfg(test)]
    pub fn defeats(&self) -> u64 {
        self.defeats
    }

    /// Times the replay disagreed with the server (or could not be made at all).
    pub fn divergences(&self) -> u64 {
        self.divergences
    }

    /// Ended battles that this machine replayed from its own battle data, event for event, with
    /// the same ending and rewards as the server's: the ones that were really CHECKED (a battle
    /// on a machine without battle data ends too, but is not counted here).
    #[cfg(test)]
    pub fn verified(&self) -> u64 {
        self.verified
    }

    /// Commands of our own character that the server applied, over the ended battles.
    #[cfg(test)]
    pub fn commands_applied(&self) -> u64 {
        self.applied.total()
    }

    /// How many requests and commands the server refused.
    pub fn rejections(&self) -> u64 {
        self.rejections
    }

    #[cfg(test)]
    pub fn last_rejection(&self) -> Option<BattleRejection> {
        self.last_rejection
    }

    /// What the server last said our account carries: no stack of zero, one stack per item, at most
    /// [`MAX_INVENTORY_STACKS`] of them (whatever was sent). Empty until it says.
    #[cfg_attr(not(test), allow(dead_code))] // for whatever shows the bag; the menu and the SMOKE line use their own views of it
    pub fn inventory(&self) -> &[InventoryEntry] {
        &self.inventory
    }

    /// How many items our account carries in all, the counts of every stack added up (0 until the
    /// server has said: the SMOKE line's `items=`).
    pub fn inventory_total(&self) -> u32 {
        self.inventory.iter().map(|e| u32::from(e.count)).sum()
    }

    /// `Inventory` messages received.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn inventory_updates(&self) -> u64 {
        self.inventory_updates
    }

    /// One line about the whole battle situation (the headless bot logs it when it finishes).
    pub fn summary(&self) -> String {
        let refusal = match self.last_rejection {
            Some(r) => format!("{} refusal(s), the last: {r}", self.rejections),
            None => "no refusals".to_string(),
        };
        let a = &self.applied;
        format!(
            "{} battle(s) entered, {} won, {} lost, {} verified by replay, {} divergence(s), {} command(s) of ours applied ({} attack, {} magic, {} item, {} defend), {refusal}",
            self.entered,
            self.victories,
            self.defeats,
            self.verified,
            self.divergences,
            a.total(),
            a.attack,
            a.magic,
            a.item,
            a.defend
        )
    }

    /// One-line descriptions of what happened since the last call, oldest first.
    pub fn drain_notes(&mut self) -> Vec<String> {
        self.notes.drain(..).collect()
    }

    /// What the server's messages did since the last call, oldest first (the events of every
    /// `BattleEvent` message, also after the battle has ended). The scene plays them as reactions.
    pub fn take_events(&mut self) -> Vec<BattleEvent> {
        self.events.drain(..).collect()
    }

    /// The replayed engine of the battle we are in, if this client could build it: who fights,
    /// their names, hit points and gauges. Read-only (it moves only with the server's messages).
    pub fn engine(&self) -> Option<&BattleState> {
        self.active.as_ref()?.replay.as_ref().map(BattleReplay::state)
    }

    /// What the scene needs to start drawing the battle we are in (`None` outside one): the
    /// formation, the party and, from this machine's own rules, where the enemies stand.
    pub fn scene_start(&self) -> Option<SceneStart> {
        let active = self.active.as_ref()?;
        Some(SceneStart {
            id: active.id,
            party: active.party.clone(),
            my_actor: active.my_actor,
            formation: self.rules.as_ref().and_then(|rules| rules.formation_scene(active.formation_id)),
        })
    }

    // ----- feeding ------------------------------------------------------------------------

    /// Apply a battle message (`EncounterStart`, `BattleEvent`, `BattleEnd`, `BattleRejected`);
    /// anything else is ignored. `now` is seconds on the local monotonic clock.
    pub fn on_message(&mut self, now: f64, msg: ServerMsg) {
        match msg {
            ServerMsg::EncounterStart {
                battle_id,
                formation_id,
                seed,
                party,
                preemptive,
                back_attack,
                battle_speed,
                ticks_per_second,
                data_fingerprint,
            } => self.on_start(
                now,
                Start {
                    battle_id,
                    setup: BattleSetup { formation_id, seed, party, preemptive, back_attack, battle_speed, ticks_per_second },
                    data_fingerprint,
                },
            ),
            ServerMsg::BattleEvent { battle_id, tick, events, command } => self.on_event(now, battle_id, tick, &events, command),
            ServerMsg::BattleEnd { battle_id, outcome, rewards } => self.on_end(now, battle_id, outcome, &rewards),
            ServerMsg::BattleRejected { battle_id, reason } => self.on_rejected(battle_id, reason),
            _ => {}
        }
    }

    fn on_start(&mut self, now: f64, start: Start) {
        if let Some(old) = self.active.take() {
            self.note(format!("{} started while {} was still running: the old one is dropped", start.battle_id, old.id));
        }
        let Start { battle_id, setup, data_fingerprint } = start;
        self.entered += 1;
        // Nothing of an earlier battle is for the scene of this one.
        self.events.clear();
        let my_actor = setup
            .party
            .iter()
            .position(|m| m.account == self.me)
            .and_then(|i| u8::try_from(i).ok())
            .map(ActorId);
        self.hud.begin(setup.formation_id, my_actor);
        let mut failure = None;
        let rules = self.rules.clone();
        let replay = match &rules {
            None => {
                self.note(format!(
                    "{battle_id} started, but this machine has no battle data (scene.bin, kernel.bin): it cannot take part or check the server"
                ));
                None
            }
            Some(rules) => {
                if rules.data_fingerprint() != data_fingerprint {
                    failure = Some(format!(
                        "this machine's battle data differs from the server's (ours {:?}, the server's {:?}): the replay cannot match",
                        rules.data_fingerprint(),
                        data_fingerprint
                    ));
                }
                match BattleReplay::start(&**rules, &setup) {
                    Ok(replay) => Some(replay),
                    Err(e) => {
                        failure = Some(format!("cannot rebuild the battle from this machine's data: {e}"));
                        None
                    }
                }
            }
        };
        let me = match my_actor {
            Some(a) => format!("you fight as actor {}", a.0),
            None => "you are not in its party".to_string(),
        };
        self.note(format!(
            "{battle_id} started: formation {}, party of {}, {me}, {} ticks/s (seed {})",
            setup.formation_id,
            setup.party.len(),
            setup.ticks_per_second,
            setup.seed
        ));
        self.active = Some(ActiveBattle {
            id: battle_id,
            formation_id: setup.formation_id,
            party: setup.party.clone(),
            replay,
            my_actor,
            ticks_per_second: setup.ticks_per_second,
            last_message: now,
            pending: None,
            diverged: false,
            messages: 0,
            my_commands: Applied::default(),
        });
        if let Some(why) = failure {
            self.diverged(why);
        }
        // The menu model outlives a battle: this battle's spells (and the inventory's items) replace
        // whatever the last one left.
        self.refresh_menu_model();
    }

    fn on_event(&mut self, now: f64, battle_id: BattleId, tick: u64, events: &[BattleEvent], command: Option<AppliedCommand>) {
        let Some(active) = self.active.as_mut().filter(|a| a.id == battle_id) else {
            self.note(format!("a battle message arrived for {battle_id}, which is not the battle we are in: ignored"));
            return;
        };
        active.last_message = now;
        active.messages += 1;
        let mine = command.filter(|c| Some(c.actor) == active.my_actor);
        let my_command = mine.is_some();
        if let Some(applied) = mine {
            active.pending = None;
            active.my_commands.count(&applied.command);
        }
        // What happened, for the scene, whether or not this machine can replay it.
        for event in events {
            if self.events.len() >= MAX_EVENTS {
                self.events.pop_front();
            }
            self.events.push_back(*event);
        }
        // Floating numbers and the menu follow the events, whether or not this machine can replay them.
        self.hud.on_events(now, events, my_command);
        let Some(replay) = active.replay.as_mut() else { return };
        if let Err(d) = replay.apply(tick, events, command.as_ref()) {
            self.diverged(format!("{battle_id} at tick {tick}: {d}"));
        }
    }

    fn on_end(&mut self, now: f64, battle_id: BattleId, outcome: Outcome, rewards: &[BattleReward]) {
        if self.active.as_ref().map(ActiveBattle::id) != Some(battle_id) {
            self.note(format!("{battle_id} ended, but it is not the battle we are in: ignored"));
            return;
        }
        let Some(active) = self.active.take() else { return };
        self.ended += 1;
        match outcome {
            Outcome::Victory => self.victories += 1,
            Outcome::Defeat => self.defeats += 1,
        }
        let mine = rewards.iter().find(|r| r.account == self.me);
        // The items the server dropped to us (it rolled them after the battle: the replay cannot check them),
        // bounded like an inventory because they come from the same untrusted place.
        let drops = mine.map(|r| normalise_inventory(r.items.clone())).unwrap_or_default();
        let line = mine.map(|r| RewardLine {
            exp: r.exp,
            gil: r.gil,
            ap: r.ap,
            items: drops.iter().map(|d| RewardItem { name: item_name(self.hud.names(), d.item), count: d.count }).collect(),
        });
        // The screen keeps this last view, with the panel, for a few seconds.
        let last = self.render(&active, now);
        self.hud.finish(now, last, outcome, line);
        let paid = match mine {
            Some(r) if drops.is_empty() => format!("you earned {} exp, {} gil, {} ap", r.exp, r.gil, r.ap),
            Some(r) => format!("you earned {} exp, {} gil, {} ap, items={}", r.exp, r.gil, r.ap, item_list(&drops)),
            None => "no reward for you".to_string(),
        };
        let mut failure = None;
        if let Some(replay) = &active.replay {
            if let Err(d) = replay.check_end(outcome, rewards) {
                failure = Some(format!("{battle_id} end: {d}"));
            }
        }
        // Verified: this machine rebuilt the battle from its OWN data and the server's whole log and
        // ending matched what that engine did. Without data, or after any divergence, it is not.
        let how = match (&active.replay, active.diverged || failure.is_some()) {
            (_, true) => "NOT verified: the replay diverged or could not be made".to_string(),
            (None, false) => "NOT verified: this machine has no battle data".to_string(),
            (Some(_), false) => {
                self.verified += 1;
                format!("verified: the server's {} message(s) replayed from this machine's own battle data and matched", active.messages)
            }
        };
        self.applied.add(active.my_commands);
        self.note(format!(
            "{battle_id} ended in {outcome:?} ({paid}); back in the field; {} command(s) of yours applied; {how}",
            active.my_commands.total()
        ));
        if let Some(why) = failure {
            // The battle is gone, so the flag has nothing to mark: count and report it.
            self.divergences += 1;
            self.note(format!("BATTLE DIVERGENCE: {why}"));
        }
    }

    /// The server told us what our account carries (protocol v6): the WHOLE inventory, on joining, after
    /// an item of ours was used in a battle and after a win that dropped items. It replaces the last one
    /// (kept bounded: see `normalise_inventory`), is noted as one line, and while a battle runs the
    /// command menu's Item list follows it. `_now` is seconds on the local monotonic clock, like
    /// [`BattleClient::on_message`]'s.
    pub fn on_inventory(&mut self, _now: f64, items: Vec<InventoryEntry>) {
        self.inventory = normalise_inventory(items);
        self.inventory_updates += 1;
        // `key=value` layout, ids and counts only: `scripts` and the tests read it.
        self.note(format!(
            "inventory: stacks={} total={} items={}",
            self.inventory.len(),
            self.inventory_total(),
            item_list(&self.inventory)
        ));
        if self.active.is_some() {
            self.refresh_menu_model();
        }
    }

    /// What the command menu offers our character, from the replayed engine and the inventory:
    ///
    /// * the spells it knows ([`BattleState::spells_for`]), each with its cost and the side it aims at,
    ///   named from this machine's book (`Spell <id>` without one);
    /// * the inventory's items that the engine can use in battle ([`BattleState::item_info`]; an item it
    ///   cannot is not listed at all), a revival aimed at the fallen, other restoring items at the party
    ///   and the rest at the enemies;
    /// * Defend.
    ///
    /// Nothing is switched off here: the menu greys a spell out by comparing its cost with the MP it is
    /// given every frame, and an item by its count, so the model never bakes in a number that moves.
    /// Without a battle, a replay (no battle data, or one that cannot be built) or a seat in the party
    /// the model is cleared: the menu then offers only Attack.
    fn refresh_menu_model(&mut self) {
        let model = self.menu_model_of_battle();
        *self.hud.model_mut() = model;
    }

    fn menu_model_of_battle(&self) -> BattleMenuModel {
        let Some(active) = self.active.as_ref() else { return BattleMenuModel::default() };
        let (Some(replay), Some(mine)) = (active.replay.as_ref(), active.my_actor) else { return BattleMenuModel::default() };
        let state = replay.state();
        let book = self.hud.names();
        let spells = state
            .spells_for(mine)
            .into_iter()
            .map(|s| MenuSpell::new(s.spell, spell_name(book, s.spell), s.mp_cost, TargetRule::from_allies(s.targets_allies)))
            .collect();
        let items = self
            .inventory
            .iter()
            .filter_map(|stack| {
                let info = state.item_info(stack.item)?;
                let target = match (info.revives, info.targets_allies) {
                    (true, _) => TargetRule::DeadAllies,
                    (false, true) => TargetRule::Allies,
                    (false, false) => TargetRule::Enemies,
                };
                Some(MenuItem::new(stack.item, item_name(book, stack.item), stack.count, target))
            })
            .collect();
        BattleMenuModel { spells, items, defend: true }
    }

    fn on_rejected(&mut self, battle_id: Option<BattleId>, reason: BattleRejection) {
        self.rejections += 1;
        self.last_rejection = Some(reason);
        if let (Some(active), Some(id)) = (self.active.as_mut(), battle_id) {
            if active.id == id {
                // The server answered the command we were waiting for.
                active.pending = None;
            }
        }
        self.note(format!("request refused: {reason}"));
    }

    /// The replay disagreed with the server: count it, mark the battle, say so loudly (the first few
    /// times; later ones are only counted).
    fn diverged(&mut self, what: String) {
        self.divergences += 1;
        if let Some(active) = self.active.as_mut() {
            active.diverged = true;
        }
        if self.divergences <= MAX_DIVERGENCE_NOTES {
            self.note(format!("BATTLE DIVERGENCE: {what}"));
        } else if self.divergences == MAX_DIVERGENCE_NOTES + 1 {
            self.note("BATTLE DIVERGENCE: further divergences are only counted".to_string());
        }
    }

    fn note(&mut self, line: String) {
        if self.notes.len() >= MAX_NOTES {
            self.notes.pop_front();
        }
        self.notes.push_back(line);
    }

    // ----- playing ------------------------------------------------------------------------

    /// Is it our own character's turn to command, as far as the replay knows (which is as far as the
    /// server has said)? Our gauge is full, the battle is not over and no command of ours is in flight:
    /// then the battle, its engine and our actor.
    fn my_turn(&self, now: f64) -> Option<(&ActiveBattle, &BattleState, ActorId)> {
        let active = self.active.as_ref()?;
        let state = active.replay.as_ref()?.state();
        let mine = active.my_actor?;
        if state.outcome().is_some() || !state.ready_actors().contains(&mine) {
            return None;
        }
        if active.pending.is_some_and(|sent| now - sent < PENDING_TIMEOUT_S) {
            return None;
        }
        Some((active, state, mine))
    }

    /// The attack our own character can make right now: it is our turn (our gauge is full as far as the
    /// replay knows, which is as far as the server has said, and no command of ours is in flight) and
    /// there is a living enemy to hit (the first one).
    pub fn attack_command(&self, now: f64) -> Option<BattleCmd> {
        let (active, state, mine) = self.my_turn(now)?;
        let target = state.enemy_ids().into_iter().find(|e| state.actor(*e).is_some_and(Combatant::is_alive))?;
        Some(BattleCmd::Command { battle_id: active.id, actor: mine, command: Command::Attack { target } })
    }

    /// Our own character healing itself, if it is our turn and it should: its HP is under 30% of its
    /// maximum and it knows a restoring spell it can pay for, the cheapest of which (then the lowest
    /// spell id) it casts on itself. A revival never qualifies: the engine refuses to aim one at a
    /// character who stands, which is what [`BattleState::check_command`] says here (it also says
    /// whether the MP is there).
    fn heal_command(&self, now: f64) -> Option<BattleCmd> {
        let (active, state, mine) = self.my_turn(now)?;
        let me = state.actor(mine)?;
        if u64::from(me.hp) * 10 >= u64::from(me.max_hp) * 3 {
            return None;
        }
        let spell = state
            .spells_for(mine)
            .into_iter()
            .filter(|s| s.targets_allies)
            .filter(|s| state.check_command(mine, &Command::Magic { spell: s.spell, target: mine }).is_ok())
            .min_by_key(|s| (s.mp_cost, s.spell))?;
        Some(BattleCmd::Command { battle_id: active.id, actor: mine, command: Command::Magic { spell: spell.spell, target: mine } })
    }

    /// What the headless bot does on our character's turn: heal itself when its HP is under 30% and a
    /// restoring spell is affordable (the cheapest one, on itself), else attack the first living enemy
    /// ([`BattleClient::attack_command`]).
    pub fn bot_command(&self, now: f64) -> Option<BattleCmd> {
        self.heal_command(now).or_else(|| self.attack_command(now))
    }

    /// Record that `cmd` was sent, so the same command is not sent again before the answer.
    pub fn note_sent(&mut self, cmd: &BattleCmd, now: f64) {
        if let (BattleCmd::Command { battle_id, .. }, Some(active)) = (cmd, self.active.as_mut()) {
            if active.id == *battle_id {
                active.pending = Some(now);
            }
        }
    }

    /// What to draw now: the battle that runs, or for a few seconds after its end the last view of
    /// the one that just ended, with its victory or defeat panel (see [`BattleClient::lingering`]);
    /// `None` otherwise. Gauges are extrapolated from the last server message for display only.
    pub fn view(&self, now: f64) -> Option<BattleView> {
        let Some(active) = self.active.as_ref() else { return self.hud.linger_view(now) };
        let mut view = self.render(active, now);
        self.hud.decorate(&mut view, now);
        Some(view)
    }

    /// Is the end of a battle still on screen (the view lingers after `BattleEnd`)? The player is
    /// already free in the field then ([`BattleClient::in_battle`] is false), but the window keeps drawing the
    /// battle and should not let the player walk until it is gone.
    pub fn lingering(&self, now: f64) -> bool {
        self.hud.lingering(now)
    }

    /// The fighters and the replay's standing, without the live parts of the UI.
    fn render(&self, active: &ActiveBattle, now: f64) -> BattleView {
        let elapsed_ticks = ((now - active.last_message).max(0.0) * f64::from(active.ticks_per_second)).min(1_000_000.0) as u64;
        let Some(replay) = active.replay.as_ref() else {
            return BattleView::unverified(active.id);
        };
        let state = replay.state();
        let bar = |id: ActorId| -> Option<Bar> {
            let c = state.actor(id)?;
            let gauge = if c.is_alive() && !c.ready {
                let inc = u64::from(state.gauge_increase(id).unwrap_or(0));
                u32::try_from(u64::from(c.gauge).saturating_add(inc.saturating_mul(elapsed_ticks)).min(u64::from(GAUGE_MAX))).unwrap_or(GAUGE_MAX)
            } else {
                c.gauge
            };
            Some(Bar {
                actor: id,
                name: c.name.clone(),
                hp: c.hp,
                max_hp: c.max_hp,
                mp: c.mp,
                max_mp: c.max_mp,
                gauge,
                ready: c.ready && c.is_alive(),
                alive: c.is_alive(),
                mine: Some(id) == active.my_actor,
            })
        };
        BattleView {
            id: active.id,
            party: state.party_ids().into_iter().filter_map(bar).collect(),
            enemies: state.enemy_ids().into_iter().filter_map(bar).collect(),
            status: if active.diverged { Status::Diverged } else { Status::Verified },
            waiting_for_answer: active.pending.is_some_and(|sent| now - sent < PENDING_TIMEOUT_S),
            ..BattleView::default()
        }
    }

    /// A key of the battle UI (see [`menu::BattleMenu`]): the menu of our character's turn takes it
    /// when one is open, and the lingering end of a battle takes `Confirm` and `Back` (closing the
    /// panel, or keeping the key from doing anything else while it cannot be closed yet).
    pub fn menu_key(&mut self, key: MenuKey, now: f64) -> MenuReply {
        if matches!(key, MenuKey::Confirm | MenuKey::Back) && self.hud.dismiss(now) {
            return MenuReply::Handled;
        }
        let Some(active) = self.active.as_ref() else { return MenuReply::Ignored };
        let view = self.render(active, now);
        match self.hud.press(key, &view) {
            Reply::Handled => MenuReply::Handled,
            Reply::Ignored => MenuReply::Ignored,
            Reply::Refused(why) => MenuReply::Refused(why.text()),
            Reply::Send(choice) => self.command_for(choice, now).map_or_else(MenuReply::Refused, MenuReply::Send),
        }
    }

    /// Esc or Backspace: goes back one level of the menu, or closes the lingering end of a battle.
    /// Returns whether it did anything; when it did not, the key is the window's to handle (and
    /// in a battle that never quits).
    pub fn back(&mut self, now: f64) -> bool {
        self.menu_key(MenuKey::Back, now) == MenuReply::Handled
    }

    /// The request for `choice` of our own character, or why there is none.
    fn command_for(&self, choice: Choice, now: f64) -> Result<BattleCmd, &'static str> {
        let active = self.active.as_ref().ok_or("there is no battle")?;
        let actor = active.my_actor.ok_or("you are not in this battle")?;
        if active.pending.is_some_and(|sent| now - sent < PENDING_TIMEOUT_S) {
            return Err("your last command is still waiting for the server");
        }
        Ok(BattleCmd::Command { battle_id: active.id, actor, command: choice.to_command() })
    }

    /// A short text for the window title while a battle runs (empty otherwise).
    pub fn title_suffix(&self, now: f64) -> String {
        let Some(view) = self.active.as_ref().map(|a| self.render(a, now)) else { return String::new() };
        let enemies = view.enemies.iter().filter(|b| b.alive).count();
        let me = match view.party.iter().find(|b| b.mine) {
            Some(b) if !b.alive => "you are down",
            Some(b) if b.ready => "YOUR TURN: choose a command",
            Some(_) => "waiting for your gauge",
            None => "watching",
        };
        let flag = match view.status {
            Status::Verified => "",
            Status::Diverged => " | DIVERGED from the server",
            Status::Unverified => " | no local battle data",
        };
        format!(" | BATTLE {} vs {enemies} enemies | {me}{flag}", view.id)
    }
}

#[cfg(test)]
mod tests;
