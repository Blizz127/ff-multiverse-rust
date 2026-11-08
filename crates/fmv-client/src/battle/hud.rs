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
//! # The banner
//!
//! Every `Action` event names what is being done, for [`BANNER_S`] seconds: a spell or an item by its
//! name from this machine's [`NameBook`] (`Spell 27` / `Item 7` without one), an enemy's own attack by
//! its scene's name (no banner when the scene has none), `Defend`; a plain attack says nothing. When
//! several actions arrive together the last one wins.
//!
//! # Numbers and the enemy list follow the blow
//!
//! The battle's state moves when a message arrives (bars, the menu, the command that is applied), but
//! the SHOW lags a little by design, never by more than [`cue::MAX_IMPACT_DELAY`]: a number floats up
//! when its blow lands ([`Popups`], scheduled with [`cue::delay_of`]), and an enemy that was beaten
//! stays in the enemy list until its collapse begins on screen ([`BattleHud::decorate`] and
//! [`BattleHud::linger_view`] keep it, at the most [`cue::MAX_DEATH_DELAY`] after the blow arrived).
//! Everything else (gauges, HP, MP, the banner, the menu) follows the state at once. The scene plays
//! its clips so that the impact frame lands at the same instants: see [`cue`].
//!
//! # The linger
//!
//! When the server ends the battle, the client drops its mirror at once (the player is free in the
//! field), but the screen keeps the last view for a few seconds: the last blow's number floats up,
//! then the panel appears. A key press closes it (after a moment, so a key still held from the
//! battle does not skip it) and so does time ([`LINGER_S`]).

use super::menu::{BattleMenu, BattleMenuModel, Ctx, Fighter, MenuKey, Reply};
use super::names::{item_name, spell_name, NameBook};
use super::panels::{Banner, Finale, RewardLine, BANNER_S, PANEL_DELAY_MS};
use super::popups::Popups;
use super::scene::cue;
use super::view::{Bar, BattleView};
use fmv_ff7_battle::rules::{ActionRef, ActorId, BattleEvent, Outcome};
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
    /// Fighters whose death is on its way to the screen, and when their collapse begins there: a beaten
    /// enemy stays in the enemy list until then.
    dying: Vec<(ActorId, f64)>,
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
        self.dying.clear();
        self.my_actor = my_actor;
        self.formation = formation;
    }

    /// The names this machine knows (spells, items, enemy attacks).
    pub fn names(&self) -> Option<&NameBook> {
        self.names.as_deref()
    }

    /// Use these names from now on (tests: the process-wide ones are installed once and for all).
    #[cfg(test)]
    pub fn set_names(&mut self, names: Option<Arc<NameBook>>) {
        self.names = names;
    }

    /// The formation of the battle that runs (`scene * 4 + slot`).
    #[cfg(test)]
    pub fn formation(&self) -> u16 {
        self.formation
    }

    /// What the menu offers: the spells, the items, whether Defend exists.
    pub fn model_mut(&mut self) -> &mut BattleMenuModel {
        &mut self.model
    }

    /// The action banner shows `text` from `now` for [`BANNER_S`] seconds.
    pub fn show_banner(&mut self, now: f64, text: String) {
        self.banner = Some((text, now));
    }

    /// The words the banner shows for `action`, if it shows any (see the module docs).
    fn action_text(&self, action: ActionRef) -> Option<String> {
        // TODO(v7): `ActionRef::Limit(id)` is named by `spell_name(self.names(), id)` (the kernel's "Names -
        // magic" table: Braver 128, Big Shot 135, Healing Wind 142, Beat Rush 149) and `ActionRef::Escape`
        // says nothing (its words belong to the panels).
        match action {
            ActionRef::Attack => None,
            ActionRef::Defend => Some("Defend".to_string()),
            ActionRef::Magic(id) => Some(spell_name(self.names(), id)),
            ActionRef::Item(id) => Some(item_name(self.names(), id)),
            ActionRef::EnemyAttack(id) => self.names()?.enemy_attack(self.formation, id).map(str::to_string),
        }
    }

    /// Battle events arrived at `now`. `my_command` says that one of them is the command of our
    /// own character being applied.
    pub fn on_events(&mut self, now: f64, events: &[BattleEvent], my_command: bool) {
        self.popups.on_events(now, events);
        self.dying.retain(|(_, due)| *due > now);
        for cue in cue::cues(events) {
            let action = cue.action.map(|(_, a)| a);
            for event in cue.events {
                if let BattleEvent::Death { actor } = *event {
                    self.dying.retain(|(who, _)| *who != actor);
                    self.dying.push((actor, now + cue::delay_of(action, event)));
                }
            }
        }
        for event in events {
            if let BattleEvent::Action { action, .. } = event {
                if let Some(text) = self.action_text(*action) {
                    self.show_banner(now, text);
                }
            }
        }
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
        // After the menu has taken its targets from the state: the list alone lingers on a corpse.
        self.hold_dying(view, now);
    }

    /// Keep the enemies whose death has not begun on screen yet in the view's list (`alive` is what
    /// the list shows; the menu has already chosen its targets). Party members are never held: their
    /// panel is the state.
    fn hold_dying(&self, view: &mut BattleView, now: f64) {
        for bar in view.enemies.iter_mut().filter(|b| !b.alive) {
            if self.dying.iter().any(|(who, due)| *who == bar.actor && now < *due) {
                bar.alive = true;
            }
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
        self.hold_dying(&mut view, now);
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
        // They appear when the blow lands, not when the event arrives (these two came with no announced
        // action: the short beat), and then rise and go with the clock.
        assert!(at(&hud, 10.0).is_empty(), "the blow has not landed yet");
        let landed = 10.0 + cue::impact_delay(None);
        assert_eq!(at(&hud, landed).len(), 2);
        assert_eq!(at(&hud, landed)[0].kind, PopupKind::Damage { amount: 42, critical: false });
        assert!(at(&hud, 12.0).is_empty(), "gone after their lifetime");
        // A new battle forgets them.
        hud.begin(1, None);
        assert!(at(&hud, landed).is_empty());
    }

    /// Us (actor 1) and a friend against two enemies, the second of them beaten.
    fn view_with_dead_enemy() -> BattleView {
        let mut v = view(false);
        v.enemies[1].alive = false;
        v.enemies[1].hp = 0;
        v
    }

    fn listed(hud: &BattleHud, now: f64) -> Vec<u8> {
        let mut v = view_with_dead_enemy();
        hud.decorate(&mut v, now);
        v.enemies.iter().filter(|b| b.alive).map(|b| b.actor.0).collect()
    }

    #[test]
    fn a_beaten_enemy_stays_in_the_list_until_its_collapse_begins_and_never_longer_than_the_cap() {
        let mut hud = hud();
        let batch = [action(0, ActionRef::Attack), BattleEvent::Damage { actor: ActorId(0), target: ActorId(3), amount: 99, critical: false }, BattleEvent::Death { actor: ActorId(3) }];
        hud.on_events(10.0, &batch, false);
        // The state says it is dead the moment the message arrived, but the list keeps it until the
        // blow has landed and it has flinched: that is when its collapse begins on the screen.
        let collapse = 10.0 + cue::impact_delay(Some(ActionRef::Attack)) + cue::DEATH_BEAT;
        assert_eq!(listed(&hud, 10.0), [2, 3], "still listed on arrival");
        assert_eq!(listed(&hud, collapse - 0.01), [2, 3]);
        assert_eq!(listed(&hud, collapse), [2], "gone when the collapse begins");
        assert!(collapse - 10.0 <= cue::MAX_DEATH_DELAY);
        // The state alone decides who else is listed: the living enemy always is, and nobody who is
        // dead without a death on its way (a scene that started late) is held.
        let other = {
            let mut v = view(false);
            v.enemies[0].alive = false;
            hud.decorate(&mut v, 10.0);
            v.enemies[0].alive
        };
        assert!(!other, "only the enemy whose death is coming is held");
        // A new battle forgets what was dying.
        hud.begin(2, None);
        assert_eq!(listed(&hud, 10.0), [2]);
    }

    #[test]
    fn holding_a_beaten_enemy_in_the_list_does_not_make_it_a_target_and_never_holds_the_party() {
        let mut hud = hud();
        hud.on_events(10.0, &[action(1, ActionRef::Attack), BattleEvent::Death { actor: ActorId(3) }, BattleEvent::Death { actor: ActorId(0) }], false);
        // Our turn: Attack, then the target selection.
        let ready = {
            let mut v = view_with_dead_enemy();
            v.party[1].ready = true;
            v.party[0].alive = false;
            v
        };
        hud.press(MenuKey::Confirm, &ready);
        let mut dressed = ready.clone();
        hud.decorate(&mut dressed, 10.1);
        let aim = dressed.menu.expect("our menu").aim.expect("the pointer");
        assert_eq!(aim.target, ActorId(2), "the pointer is on the living enemy, not on the one still listed");
        assert!(dressed.enemies[1].alive, "though the list still shows it");
        assert!(!dressed.party[0].alive, "a fallen party member is shown fallen at once: the panel is the state");
    }

    #[test]
    fn the_last_enemy_is_still_listed_in_the_linger_view_until_its_collapse_begins() {
        let mut hud = hud();
        hud.on_events(20.0, &[action(1, ActionRef::Attack), BattleEvent::Damage { actor: ActorId(1), target: ActorId(2), amount: 9, critical: true }, BattleEvent::Death { actor: ActorId(2) }, BattleEvent::Victory], true);
        let mut last = view(false);
        last.enemies[0].alive = false;
        last.enemies[1].alive = false;
        hud.finish(20.0, last, Outcome::Victory, None);
        let listed_at = |now: f64| hud.linger_view(now).expect("lingering").enemies.iter().filter(|b| b.alive).map(|b| b.actor.0).collect::<Vec<_>>();
        assert_eq!(listed_at(20.1), [2], "the beaten one is still there");
        assert!(listed_at(20.0 + cue::MAX_DEATH_DELAY + 0.01).is_empty(), "then the list is empty for the panel");
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

    /// A hud for formation 301 (scene 75) whose book names Cure (0), Fire (27), Phoenix Down (item 7)
    /// and attack 272 of scene 75.
    fn hud_with_names() -> BattleHud {
        let mut spells = vec![String::new(); 28];
        spells[0] = "Cure".to_string();
        spells[27] = "Fire".to_string();
        let mut items = vec![String::new(); 8];
        items[7] = "Phoenix Down".to_string();
        let book = NameBook::from_tables(&spells, &items).with_enemy_attack(75, 272, "Machine Gun");
        let mut hud = BattleHud::new(Some(Arc::new(book)));
        hud.begin(301, Some(ActorId(1)));
        hud
    }

    fn action(actor: u8, what: ActionRef) -> BattleEvent {
        BattleEvent::Action { actor: ActorId(actor), action: what }
    }

    fn banner_text(hud: &BattleHud, now: f64) -> Option<String> {
        let mut v = view(false);
        hud.decorate(&mut v, now);
        v.banner.map(|b| b.text)
    }

    #[test]
    fn an_action_shows_its_name_in_the_banner() {
        for (what, want) in [
            (ActionRef::Magic(27), "Fire"),
            (ActionRef::Item(7), "Phoenix Down"),
            (ActionRef::EnemyAttack(272), "Machine Gun"),
            (ActionRef::Defend, "Defend"),
            // An id the book does not name gets the plain label a menu would show.
            (ActionRef::Magic(5), "Spell 5"),
            (ActionRef::Item(99), "Item 99"),
        ] {
            let mut hud = hud_with_names();
            hud.on_events(5.0, &[action(1, what)], false);
            assert_eq!(banner_text(&hud, 5.1).as_deref(), Some(want), "{what:?}");
            assert_eq!(banner_text(&hud, 5.0 + BANNER_S + 0.01), None, "{what:?} goes by itself");
        }
    }

    #[test]
    fn a_plain_attack_and_an_enemy_attack_without_a_name_make_no_banner() {
        let mut hud = hud_with_names();
        for what in [ActionRef::Attack, ActionRef::EnemyAttack(999)] {
            hud.on_events(5.0, &[action(2, what)], false);
            assert_eq!(banner_text(&hud, 5.1), None, "{what:?}");
        }
        // Nor does an enemy attack of ANOTHER scene's table: attack 272 is named by scene 75 only.
        hud.begin(8, Some(ActorId(1)));
        hud.on_events(5.0, &[action(2, ActionRef::EnemyAttack(272))], false);
        assert_eq!(banner_text(&hud, 5.1), None);
        // They do not wipe a banner that is still up, either.
        let mut hud = hud_with_names();
        hud.on_events(5.0, &[action(1, ActionRef::Magic(27))], false);
        hud.on_events(5.2, &[action(2, ActionRef::Attack), action(2, ActionRef::EnemyAttack(999))], false);
        assert_eq!(banner_text(&hud, 5.3).as_deref(), Some("Fire"));
    }

    #[test]
    fn the_last_action_of_a_batch_wins_the_banner() {
        let mut hud = hud_with_names();
        hud.on_events(5.0, &[action(1, ActionRef::Magic(27)), action(2, ActionRef::EnemyAttack(272)), action(1, ActionRef::Item(7))], false);
        assert_eq!(banner_text(&hud, 5.1).as_deref(), Some("Phoenix Down"));
        // A later batch replaces it and restarts its time.
        hud.on_events(6.0, &[action(2, ActionRef::EnemyAttack(272))], false);
        let mut v = view(false);
        hud.decorate(&mut v, 6.25);
        assert_eq!(v.banner, Some(Banner { text: "Machine Gun".into(), age_ms: 250 }));
        // Actions are not damage: they make no number over anybody.
        assert!(v.popups.is_empty());
    }

    #[test]
    fn without_a_book_the_banner_uses_plain_labels_and_enemy_attacks_stay_unnamed() {
        let mut hud = BattleHud::new(None);
        hud.begin(301, Some(ActorId(1)));
        for (what, want) in [
            (ActionRef::Magic(27), Some("Spell 27")),
            (ActionRef::Item(7), Some("Item 7")),
            (ActionRef::EnemyAttack(272), None),
            (ActionRef::Defend, Some("Defend")),
            (ActionRef::Attack, None),
        ] {
            let mut hud = hud.clone();
            hud.on_events(5.0, &[action(1, what)], false);
            assert_eq!(banner_text(&hud, 5.1).as_deref(), want, "{what:?}");
        }
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
        assert!(first.popups.is_empty(), "the last blow has not landed yet");
        let landed = hud.linger_view(20.0 + cue::impact_delay(None) + 0.05).expect("lingering");
        assert_eq!(landed.popups.len(), 1, "the last blow's number floats up when it lands, in the linger too");
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
