//! The live state of the battle UI that the battle itself does not keep: the command menu and what
//! it offers, the floating numbers, the action banner and the lingering end of the battle (the
//! last view with its victory or defeat panel). One [`BattleHud`] per [`super::BattleClient`].
//!
//! The client feeds it as messages arrive ([`BattleHud::begin`], [`BattleHud::on_events`],
//! [`BattleHud::finish`]), asks it to dress the view it built ([`BattleHud::decorate`]) and hands it
//! the player's keys ([`BattleHud::press`], [`BattleHud::dismiss`]). Time is always the `now` the
//! caller passes (seconds on the client's clock): nothing here reads a clock, so it is testable
//! without waiting.
//!
//! # The menu opens by itself
//!
//! Our character's menu exists exactly while it is our turn: our gauge is full, we are alive, no
//! command of ours is waiting for the server's answer and the battle is not over. It starts at the
//! command list every time our gauge fills (the cursor stays on the command used last), and it is
//! reset when a command of ours is applied or our character falls.
//!
//! # The linger
//!
//! When the server ends the battle, the client drops its mirror at once (the player is free in the
//! field), but the screen keeps the last view for a few seconds: the last blow's number floats up,
//! then the panel appears. A key press closes it (after a moment, so a key still held from the
//! battle does not skip it) and so does time ([`LINGER_S`]).

use super::menu::{BattleMenu, BattleMenuModel, Ctx, Fighter, MenuKey, Reply};
use super::names::NameBook;
use super::panels::{Banner, Finale, RewardLine, BANNER_S, PANEL_DELAY_MS};
use super::popups::Popups;
use super::view::{Bar, BattleView};
use fmv_ff7_battle::rules::{ActorId, BattleEvent, Outcome};
use std::sync::Arc;

/// How long the last view of a battle stays on screen after its end, in seconds.
pub const LINGER_S: f64 = 8.0;
/// A key closes the end-of-battle panel only this long after the battle ended: the panel itself
/// appears at [`PANEL_DELAY_MS`], and a key pressed in the moment before it must not skip it.
const DISMISS_AFTER_S: f64 = PANEL_DELAY_MS as f64 / 1000.0 + 0.4;

/// The battle that just ended, as the screen lingers on it.
#[derive(Debug, Clone)]
struct Ended {
    view: BattleView,
    outcome: Outcome,
    reward: Option<RewardLine>,
    at: f64,
    dismissed: bool,
}

/// See the module docs.
#[derive(Debug, Clone, Default)]
pub struct BattleHud {
    menu: BattleMenu,
    model: BattleMenuModel,
    popups: Popups,
    /// The action banner and when it appeared.
    banner: Option<(String, f64)>,
    my_actor: Option<ActorId>,
    /// The formation of the battle (`scene * 4 + slot`): which scene names an enemy's attacks.
    formation: u16,
    names: Option<Arc<NameBook>>,
    ended: Option<Ended>,
}

/// The party members and enemies of `bars` as the menu sees them.
fn fighters(bars: &[Bar]) -> Vec<Fighter> {
    bars.iter().map(|b| Fighter { id: b.actor, alive: b.alive }).collect()
}

/// Our character's MP if it is our turn to command: our gauge is full, we are alive, no command of
/// ours waits for an answer and the battle is not over.
fn turn_mp(view: &BattleView) -> Option<u32> {
    if view.waiting_for_answer || view.finale.is_some() {
        return None;
    }
    view.party.iter().find(|b| b.mine && b.ready && b.alive).map(|b| b.mp)
}

impl BattleHud {
    /// A hud with this machine's names (`None`: spells and items get plain labels).
    pub fn new(names: Option<Arc<NameBook>>) -> Self {
        Self { names, ..Self::default() }
    }

    /// A new battle starts: forget everything of the last one. `my_actor` is our character,
    /// `formation` the battle's formation id.
    pub fn begin(&mut self, formation: u16, my_actor: Option<ActorId>) {
        self.menu.reset();
        self.popups.clear();
        self.banner = None;
        self.ended = None;
        self.my_actor = my_actor;
        self.formation = formation;
    }

    /// The names this machine knows (spells, items, enemy attacks).
    #[cfg_attr(not(test), allow(dead_code))] // the integrator names the engine's spells and actions with it
    pub fn names(&self) -> Option<&NameBook> {
        self.names.as_deref()
    }

    /// The formation of the battle that runs (`scene * 4 + slot`).
    #[cfg_attr(not(test), allow(dead_code))] // the integrator names an enemy's attack through its scene
    pub fn formation(&self) -> u16 {
        self.formation
    }

    /// What the menu offers: the spells, the items, whether Defend exists.
    #[cfg_attr(not(test), allow(dead_code))] // the integrator fills it from the engine and the inventory
    pub fn model_mut(&mut self) -> &mut BattleMenuModel {
        &mut self.model
    }

    /// The action banner shows `text` from `now` for [`BANNER_S`] seconds.
    #[cfg_attr(not(test), allow(dead_code))] // the integrator calls it for the engine's `Action` event
    pub fn show_banner(&mut self, now: f64, text: String) {
        self.banner = Some((text, now));
    }

    /// Battle events arrived at `now`. `my_command` says that one of them is the command of our
    /// own character being applied.
    pub fn on_events(&mut self, now: f64, events: &[BattleEvent], my_command: bool) {
        self.popups.on_events(now, events);
        let mine = |actor: &ActorId| Some(*actor) == self.my_actor;
        let own_turn_starts_or_ends = events.iter().any(|e| match e {
            BattleEvent::GaugeReady { actor } | BattleEvent::Death { actor } => mine(actor),
            _ => false,
        });
        if my_command || own_turn_starts_or_ends {
            self.menu.reset();
        }
    }

    /// Dress `view`, the state of a battle that runs, with the live parts of the UI at `now`.
    pub fn decorate(&self, view: &mut BattleView, now: f64) {
        view.popups = self.popups.views(now);
        view.banner = self.banner.as_ref().filter(|(_, at)| now - at < BANNER_S).map(|(text, at)| Banner {
            text: text.clone(),
            age_ms: ((now - at).max(0.0) * 1000.0).round() as u32,
        });
        if let Some(mp) = turn_mp(view) {
            let (allies, enemies) = (fighters(&view.party), fighters(&view.enemies));
            view.menu = Some(self.menu.view(&Ctx { model: &self.model, allies: &allies, enemies: &enemies, mp }));
        }
    }

    /// A key for the menu, if it is open (`view` is the state of the battle now).
    pub fn press(&mut self, key: MenuKey, view: &BattleView) -> Reply {
        let Some(mp) = turn_mp(view) else { return Reply::Ignored };
        let (allies, enemies) = (fighters(&view.party), fighters(&view.enemies));
        self.menu.press(key, &Ctx { model: &self.model, allies: &allies, enemies: &enemies, mp })
    }

    /// The battle ended at `now`: keep `last` (its final view), the outcome and what our character
    /// earned on screen for a while.
    pub fn finish(&mut self, now: f64, last: BattleView, outcome: Outcome, reward: Option<RewardLine>) {
        self.menu.reset();
        self.banner = None;
        self.ended = Some(Ended { view: last, outcome, reward, at: now, dismissed: false });
    }

    /// Is the end of a battle still on screen at `now`?
    pub fn lingering(&self, now: f64) -> bool {
        self.ended.as_ref().is_some_and(|e| !e.dismissed && now - e.at < LINGER_S)
    }

    /// The last view of the battle that ended, with its panel, while it lingers.
    pub fn linger_view(&self, now: f64) -> Option<BattleView> {
        let ended = self.ended.as_ref().filter(|_| self.lingering(now))?;
        let age = (now - ended.at).max(0.0);
        let mut view = ended.view.clone();
        view.popups = self.popups.views(now);
        view.menu = None;
        view.banner = None;
        view.finale = Some(Finale {
            outcome: ended.outcome,
            reward: ended.reward.clone(),
            age_ms: (age * 1000.0).round() as u32,
            dismissible: age >= DISMISS_AFTER_S,
        });
        Some(view)
    }

    /// A key that closes the lingering end of a battle. Returns whether there was one to consume
    /// the key (it is consumed even while the panel cannot be closed yet, so that it does not do
    /// anything else, like quit).
    pub fn dismiss(&mut self, now: f64) -> bool {
        if !self.lingering(now) {
            return false;
        }
        if let Some(ended) = self.ended.as_mut() {
            if now - ended.at >= DISMISS_AFTER_S {
                ended.dismissed = true;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::battle::menu::{Choice, Level, MenuItem, MenuSpell, TargetRule};
    use crate::battle::popups::PopupKind;
    use crate::battle::view::Status;
    use fmv_protocol::BattleId;

    fn bar(actor: u8, mine: bool, ready: bool, alive: bool) -> Bar {
        Bar { actor: ActorId(actor), name: format!("F{actor}"), hp: if alive { 10 } else { 0 }, max_hp: 10, mp: 12, max_mp: 20, ready, alive, mine, ..Bar::default() }
    }

    /// Us (actor 1) and a friend against two enemies.
    fn view(ready: bool) -> BattleView {
        BattleView {
            id: BattleId(1),
            party: vec![bar(0, false, false, true), bar(1, true, ready, true)],
            enemies: vec![bar(2, false, false, true), bar(3, false, false, true)],
            status: Status::Verified,
            ..BattleView::default()
        }
    }

    fn hud() -> BattleHud {
        let mut hud = BattleHud::new(None);
        hud.begin(7, Some(ActorId(1)));
        hud.model_mut().spells = vec![MenuSpell::new(5, "Fire", 4, TargetRule::Enemies)];
        hud.model_mut().items = vec![MenuItem::new(6, "Potion", 2, TargetRule::Allies)];
        hud
    }

    #[test]
    fn the_menu_exists_only_on_our_turn() {
        let hud = hud();
        let dressed = |v: &BattleView| {
            let mut v = v.clone();
            hud.decorate(&mut v, 1.0);
            v.menu
        };
        assert!(dressed(&view(false)).is_none(), "gauge not full");
        assert!(dressed(&view(true)).is_some(), "gauge full");
        let mut waiting = view(true);
        waiting.waiting_for_answer = true;
        assert!(dressed(&waiting).is_none(), "a command of ours is waiting for the server");
        let mut down = view(true);
        down.party[1].alive = false;
        assert!(dressed(&down).is_none(), "our character is down");
        let mut other = view(true);
        other.party[1].mine = false;
        assert!(dressed(&other).is_none(), "somebody else's turn");
        let mut over = view(true);
        over.finale = Some(Finale { outcome: Outcome::Victory, reward: None, age_ms: 0, dismissible: false });
        assert!(dressed(&over).is_none(), "the battle is over");
    }

    #[test]
    fn keys_reach_the_menu_only_on_our_turn_and_choices_come_out_with_actor_ids() {
        let mut hud = hud();
        assert_eq!(hud.press(MenuKey::Down, &view(false)), Reply::Ignored, "no menu, the key is not ours");
        assert_eq!(hud.press(MenuKey::Back, &view(false)), Reply::Ignored);
        let v = view(true);
        assert_eq!(hud.press(MenuKey::Confirm, &v), Reply::Handled, "Attack opens the target selection");
        let mut dressed = v.clone();
        hud.decorate(&mut dressed, 1.0);
        assert_eq!(dressed.menu.as_ref().map(|m| m.level), Some(Level::Target));
        assert_eq!(hud.press(MenuKey::Confirm, &v), Reply::Send(Choice::Attack { target: ActorId(2) }));
        // Magic with the model: Fire on the second enemy.
        for k in [MenuKey::Down, MenuKey::Confirm, MenuKey::Confirm, MenuKey::Right] {
            hud.press(k, &v);
        }
        assert_eq!(hud.press(MenuKey::Confirm, &v), Reply::Send(Choice::Magic { spell: 5, target: ActorId(3) }));
    }

    #[test]
    fn the_menu_starts_over_when_our_gauge_fills_a_command_is_applied_or_we_fall() {
        let v = view(true);
        for (event, my_command) in [(BattleEvent::GaugeReady { actor: ActorId(1) }, false), (BattleEvent::Death { actor: ActorId(1) }, false), (BattleEvent::Victory, true)] {
            let mut hud = hud();
            hud.press(MenuKey::Down, &v);
            hud.press(MenuKey::Confirm, &v);
            hud.press(MenuKey::Confirm, &v);
            hud.on_events(1.0, &[event], my_command);
            let mut dressed = v.clone();
            hud.decorate(&mut dressed, 1.0);
            assert_eq!(dressed.menu.map(|m| m.level), Some(Level::Commands), "{event:?}");
        }
        // Somebody else's gauge or death does not disturb our menu.
        let mut hud = hud();
        hud.press(MenuKey::Down, &v);
        hud.press(MenuKey::Confirm, &v);
        hud.on_events(1.0, &[BattleEvent::GaugeReady { actor: ActorId(0) }, BattleEvent::Death { actor: ActorId(2) }], false);
        let mut dressed = v.clone();
        hud.decorate(&mut dressed, 1.0);
        assert_eq!(dressed.menu.map(|m| m.level), Some(Level::Magic));
    }

    #[test]
    fn events_make_popups_that_come_and_go_with_the_clock() {
        let mut hud = hud();
        hud.on_events(10.0, &[BattleEvent::Damage { actor: ActorId(1), target: ActorId(2), amount: 42, critical: false }, BattleEvent::Miss { actor: ActorId(2), target: ActorId(1) }], false);
        let at = |hud: &BattleHud, now: f64| {
            let mut v = view(false);
            hud.decorate(&mut v, now);
            v.popups
        };
        assert_eq!(at(&hud, 10.0).len(), 2);
        assert_eq!(at(&hud, 10.0)[0].kind, PopupKind::Damage { amount: 42, critical: false });
        assert!(at(&hud, 12.0).is_empty(), "gone after their lifetime");
        // A new battle forgets them.
        hud.begin(1, None);
        assert!(at(&hud, 10.0).is_empty());
    }

    #[test]
    fn the_banner_shows_for_its_time_and_is_gone() {
        let mut hud = hud();
        let at = |hud: &BattleHud, now: f64| {
            let mut v = view(false);
            hud.decorate(&mut v, now);
            v.banner
        };
        assert!(at(&hud, 5.0).is_none());
        hud.show_banner(5.0, "Fire".into());
        assert_eq!(at(&hud, 5.5), Some(Banner { text: "Fire".into(), age_ms: 500 }));
        assert!(at(&hud, 5.0 + BANNER_S + 0.001).is_none());
        hud.show_banner(9.0, "Bite".into());
        hud.begin(1, None);
        assert!(at(&hud, 9.1).is_none(), "a new battle clears it");
    }

    fn ended_hud() -> BattleHud {
        let mut hud = hud();
        hud.on_events(20.0, &[BattleEvent::Damage { actor: ActorId(1), target: ActorId(2), amount: 99, critical: true }], true);
        hud.finish(20.0, view(false), Outcome::Victory, Some(RewardLine { exp: 5, gil: 6, ap: 7, items: vec![] }));
        hud
    }

    #[test]
    fn the_end_of_a_battle_lingers_with_its_last_numbers_then_its_panel() {
        let hud = ended_hud();
        assert!(hud.lingering(20.0) && hud.lingering(20.0 + LINGER_S - 0.01));
        assert!(!hud.lingering(20.0 + LINGER_S), "and then it is over");
        let first = hud.linger_view(20.1).expect("lingering");
        let f = first.finale.as_ref().expect("the finale");
        assert_eq!((f.outcome, f.age_ms, f.dismissible), (Outcome::Victory, 100, false));
        assert_eq!(f.reward, Some(RewardLine { exp: 5, gil: 6, ap: 7, items: vec![] }));
        assert_eq!(first.popups.len(), 1, "the last blow's number is still floating");
        assert!(first.menu.is_none() && first.banner.is_none());
        assert_eq!(first.party.len(), 2, "the last view of the fighters");
        assert!(hud.linger_view(20.0 + LINGER_S).is_none());
        let later = hud.linger_view(23.0).unwrap();
        assert!(later.finale.unwrap().dismissible && later.popups.is_empty());
    }

    #[test]
    fn a_key_closes_the_panel_only_after_its_moment_and_is_consumed_before() {
        let mut hud = ended_hud();
        assert!(hud.dismiss(20.5), "consumed: it must not quit or attack");
        assert!(hud.lingering(20.6), "but too early to close it");
        assert!(hud.dismiss(20.0 + DISMISS_AFTER_S));
        assert!(!hud.lingering(20.0 + DISMISS_AFTER_S), "closed");
        assert!(!hud.dismiss(21.5), "nothing left to consume the key");
        assert!(hud.linger_view(21.5).is_none());
        // With nothing ended at all, nothing is consumed.
        assert!(!BattleHud::new(None).dismiss(1.0));
    }

    #[test]
    fn a_new_battle_replaces_the_lingering_one() {
        let mut hud = ended_hud();
        hud.begin(3, Some(ActorId(0)));
        assert!(!hud.lingering(20.1) && hud.linger_view(20.1).is_none());
        assert_eq!(hud.formation(), 3);
    }

    #[test]
    fn names_are_what_the_client_installed() {
        assert!(BattleHud::new(None).names().is_none());
        let book = NameBook::from_tables(&["Bolt".to_string()], &[]);
        let hud = BattleHud::new(Some(Arc::new(book)));
        assert_eq!(hud.names().map(|n| n.spell(0)), Some("Bolt".to_string()));
    }
}
