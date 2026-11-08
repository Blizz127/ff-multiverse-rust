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
//! * [`BattleView`] / [`battle_vertices`] draw the battle as flat coloured rectangles (the window),
//!   [`draw_battle_text`] adds names, HP numbers and the turn hint in the UI font over them.
//! * [`BattleBot`] is the headless `--battle-auto` player.
//! * [`open_rules`] reads this machine's battle data (the same library set as the field archive).
//!
//! While a battle runs the player is frozen in the field: loops neither walk nor send movement.
//! The mirror advances ONLY with server messages (so it can never run ahead of the server); the
//! ATB gauges in the view are extrapolated between messages for display and snap back at the next
//! one. Everything from the server is untrusted: it is bounds-checked by the replay and nothing
//! here can panic on it.

mod bot;
mod data;
mod hud;
mod menu;
mod names;
mod panels;
mod popups;
mod text;
mod view;

pub use bot::BattleBot;
pub use data::open_rules;
pub use menu::MenuKey;
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
use fmv_ff7_battle::rules::{Combatant, GAUGE_MAX};
use hud::BattleHud;
use menu::{Choice, Reply};
use panels::RewardLine;
use fmv_protocol::battle::{BattleEvent, Command, Outcome};
use fmv_protocol::{
    ActorId, AppliedCommand, BattleId, BattleRejection, BattleReplay, BattleReward, BattleRules, BattleSetup, ClientMsg, ServerMsg,
};
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
#[allow(dead_code)] // `main` calls it next to `install_local_rules`
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

/// One battle this client is in.
#[derive(Debug)]
pub struct ActiveBattle {
    id: BattleId,
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
    /// Commands of OUR character that the server applied (it echoes each one in its event).
    my_commands: u64,
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
    /// Commands of our character the server applied, over the ended battles.
    commands_applied: u64,
    rejections: u64,
    last_rejection: Option<BattleRejection>,
    notes: VecDeque<String>,
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
            commands_applied: 0,
            rejections: 0,
            last_rejection: None,
            notes: VecDeque::new(),
            hud: BattleHud::new(local_names()),
        }
    }

    /// Replace the battle rules this client mirrors with (tests; a running client uses the
    /// process-wide ones).
    #[cfg(test)]
    pub fn set_rules(&mut self, rules: Option<Arc<dyn BattleRules>>) {
        self.rules = rules;
    }

    /// What the command menu offers: the spells, the items, whether Defend exists. Empty (only
    /// Attack) until the integrator fills it from the engine's spell list and the inventory.
    #[cfg_attr(not(test), allow(dead_code))] // filled where the engine's spells and the inventory arrive
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
        self.commands_applied
    }

    /// How many requests and commands the server refused.
    pub fn rejections(&self) -> u64 {
        self.rejections
    }

    #[cfg(test)]
    pub fn last_rejection(&self) -> Option<BattleRejection> {
        self.last_rejection
    }

    /// One line about the whole battle situation (the headless bot logs it when it finishes).
    pub fn summary(&self) -> String {
        let refusal = match self.last_rejection {
            Some(r) => format!("{} refusal(s), the last: {r}", self.rejections),
            None => "no refusals".to_string(),
        };
        format!(
            "{} battle(s) entered, {} won, {} lost, {} verified by replay, {} divergence(s), {} command(s) of ours applied, {refusal}",
            self.entered, self.victories, self.defeats, self.verified, self.divergences, self.commands_applied
        )
    }

    /// One-line descriptions of what happened since the last call, oldest first.
    pub fn drain_notes(&mut self) -> Vec<String> {
        self.notes.drain(..).collect()
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
            replay,
            my_actor,
            ticks_per_second: setup.ticks_per_second,
            last_message: now,
            pending: None,
            diverged: false,
            messages: 0,
            my_commands: 0,
        });
        if let Some(why) = failure {
            self.diverged(why);
        }
    }

    fn on_event(&mut self, now: f64, battle_id: BattleId, tick: u64, events: &[BattleEvent], command: Option<AppliedCommand>) {
        let Some(active) = self.active.as_mut().filter(|a| a.id == battle_id) else {
            self.note(format!("a battle message arrived for {battle_id}, which is not the battle we are in: ignored"));
            return;
        };
        active.last_message = now;
        active.messages += 1;
        let my_command = command.is_some_and(|c| Some(c.actor) == active.my_actor);
        if my_command {
            active.pending = None;
            active.my_commands += 1;
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
        // The screen keeps this last view, with the panel, for a few seconds.
        let last = self.render(&active, now);
        self.hud.finish(now, last, outcome, mine.map(|r| RewardLine { exp: r.exp, gil: r.gil, ap: r.ap, items: Vec::new() }));
        let paid = match mine {
            Some(r) => format!("you earned {} exp, {} gil, {} ap", r.exp, r.gil, r.ap),
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
        self.commands_applied += active.my_commands;
        self.note(format!(
            "{battle_id} ended in {outcome:?} ({paid}); back in the field; {} command(s) of yours applied; {how}",
            active.my_commands
        ));
        if let Some(why) = failure {
            // The battle is gone, so the flag has nothing to mark: count and report it.
            self.divergences += 1;
            self.note(format!("BATTLE DIVERGENCE: {why}"));
        }
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

    /// The attack our own character can make right now: our gauge is full (as far as the replay
    /// knows, which is as far as the server has said), no command of ours is in flight, and there
    /// is a living enemy to hit (the first one).
    pub fn attack_command(&self, now: f64) -> Option<BattleCmd> {
        let active = self.active.as_ref()?;
        let state = active.replay.as_ref()?.state();
        let mine = active.my_actor?;
        if state.outcome().is_some() || !state.ready_actors().contains(&mine) {
            return None;
        }
        if active.pending.is_some_and(|sent| now - sent < PENDING_TIMEOUT_S) {
            return None;
        }
        let target = state.enemy_ids().into_iter().find(|e| state.actor(*e).is_some_and(Combatant::is_alive))?;
        Some(BattleCmd::Command { battle_id: active.id, actor: mine, command: Command::Attack { target } })
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
        let command = choice.to_command().ok_or("that command is not available yet")?;
        Ok(BattleCmd::Command { battle_id: active.id, actor, command })
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
