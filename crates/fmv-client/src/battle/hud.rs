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
//! The menu exists exactly while it is the turn of the member it is for, the FOCUS: the first of the members we command
//! (our own character and, when the roster names us their commander, the canon companions: protocol v17) whose gauge is
//! full, who is alive and able to act, while no command of ours is waiting for the server's answer and the battle is not
//! over. It starts at the command list every time its member's gauge fills (the cursor stays on the command used last), and
//! it is reset when a command of that member is applied or that member falls. Another member's gauge filling never takes
//! the menu away from a choice being made: the client says who has it ([`BattleHud::set_focus`], which starts a different
//! member's menu at the command list), and the focus is our own character when none of ours is ready.
//!
//! # The banner
//!
//! Every `Action` event names what is being done, for [`BANNER_S`] seconds: a spell or an item by its
//! name from this machine's [`NameBook`] (`Spell 27` / `Item 7` without one), an enemy's own attack by
//! its scene's name (no banner when the scene has none), `Defend`; a plain attack says nothing. Each name
//! comes up when its performance BEGINS ([`cue::Cue::start`]): the actions of one batch are played one after
//! the other (a counter after the blow that provoked it), and the banner names the one that is being played, the
//! last one begun winning.
//!
//! # Numbers, HP and the enemy list follow the blow; so do the sounds
//!
//! The battle's state moves when a message arrives (bars, the menu, the command that is applied), but
//! the SHOW lags a little by design, by [`cue::MAX_IMPACT_DELAY`] at the most for the first blow of a batch
//! (and for a counter, or any later action of it, until the one before has played, see [`cue`]): a number floats up
//! when its blow lands ([`Popups`], scheduled with [`cue::Cue::delay_of`]), the HP and MP of the panels and the names of
//! the fallen change at that same instant, and so do the statuses the panels tag them with and the colours of the 3D bodies
//! ([`Lag`]: the view is the engine's numbers minus the changes that have not
//! landed), and an enemy that was beaten stays in the enemy list until its collapse begins on screen
//! ([`BattleHud::decorate`] and [`BattleHud::linger_view`] keep it, at the most [`cue::MAX_BATCH_SPAN`] after the batch
//! arrived). The sounds of the performances ([`Sounds`]: a weapon's hit, critical hit and miss, an enemy attack's
//! impact) are scheduled at the very same instants, and [`BattleHud::sounds_due`] hands the audio the ones that came due.
//! The menu reads the engine's own numbers (it is made before the lag is applied), and everything else (gauges, the
//! banner) follows the state at once. The scene plays its clips so that the impact frame lands at the same instants: see
//! [`cue`].
//!
//! # The linger
//!
//! When the server ends the battle, the client drops its mirror at once (the player is free in the
//! field), but the screen keeps the last view for a few seconds: the last blow's number floats up,
//! then the panel appears. The panel counts from the moment the LAST performance began on screen (a counter that
//! finishes the party begins after the blow that provoked it, so that is later than the message that ended the
//! battle): the blow's number is seen before the panel comes. A key press closes it (after a moment, so a key
//! still held from the battle does not skip it) and so does time ([`LINGER_S`]).

use super::lag::{Lag, Vitals};
use super::menu::{BattleMenu, BattleMenuModel, Ctx, Fighter, Level, MenuKey, Reply};
use super::names::{item_name, limit_name, spell_name, NameBook};
use super::panels::{Banner, Finale, RewardLine, BANNER_S, PANEL_DELAY_MS};
use super::popups::Popups;
use super::scene::cue;
use super::sounds::{BattleSound, Palette, Planned, Sounds};
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
    /// The action banners and when each comes up (in the order they were scheduled; the one on screen is the last that
    /// has begun, see [`BattleHud::decorate`]). A few are kept: a batch has a handful of performances at most.
    banners: Vec<(String, f64)>,
    /// The member the menu is for (see the module docs, "The menu opens by itself"): our own character at first.
    my_actor: Option<ActorId>,
    /// The formation of the battle (`scene * 4 + slot`): which scene names an enemy's attacks.
    formation: u16,
    names: Option<Arc<NameBook>>,
    ended: Option<Ended>,
    /// Fighters whose death is on its way to the screen, and when their collapse begins there: a beaten
    /// enemy stays in the enemy list until then.
    dying: Vec<(ActorId, f64)>,
    /// When the last performance of everything that arrived begins on screen (the clock of [`BattleHud::on_events`] plus
    /// [`cue::Cue::start`]): the battle's end counts from here when it comes later than the message that ended it.
    last_start: f64,
    /// The HP, MP and standing that have not landed yet (see [`Lag`]). Not cleared by the end of the battle: the last
    /// blow lands after the message that ended it.
    lag: Lag,
    /// What this battle sounds like (its party's weapons, its formation's attacks) and the sounds waiting for their
    /// moment. Also kept past the end of the battle, for the same reason.
    palette: Palette,
    sounds: Sounds,
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

    /// A new battle starts: forget everything of the last one. `my_actor` is our character (the member the menu is for until
    /// [`BattleHud::set_focus`] says another), `formation` the battle's formation id.
    pub fn begin(&mut self, formation: u16, my_actor: Option<ActorId>) {
        self.menu.reset();
        self.popups.clear();
        self.banners.clear();
        self.ended = None;
        self.dying.clear();
        self.lag.clear();
        self.sounds.clear();
        self.palette = Palette::default();
        self.last_start = f64::NEG_INFINITY;
        self.my_actor = my_actor;
        self.formation = formation;
    }

    /// The menu is for `actor` from now on (`None`: nobody). A different member starts at the command list.
    pub fn set_focus(&mut self, actor: Option<ActorId>) {
        if self.my_actor != actor {
            self.my_actor = actor;
            self.menu.reset();
        }
    }

    /// Is the menu at its command list (not in a Magic or Item list, nor choosing a target)?
    pub fn menu_at_commands(&self) -> bool {
        self.menu.level() == Level::Commands
    }

    /// What the battle that just began sounds like (see [`Palette::new`]).
    pub fn set_palette(&mut self, palette: Palette) {
        self.palette = palette;
    }

    /// The sounds this battle can need, to have them read before they are due (`spells`: the kernel attack indexes of the
    /// spells in reach).
    pub fn sound_plan(&self, spells: &[u16]) -> Vec<Planned> {
        self.palette.plan(spells)
    }

    /// The sounds of this battle that are due in `from <= at < to` (seconds on the client's clock), with the time each was due.
    /// Asking for consecutive windows gives each once; it answers also after the battle has ended.
    pub fn sounds_due(&self, from: f64, to: f64) -> Vec<(f64, BattleSound)> {
        self.sounds.due(from, to)
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

    /// The action banner shows `text` from `at` for [`BANNER_S`] seconds (a performance that has not begun yet leaves the
    /// banner of the one on screen alone until it does).
    pub fn show_banner(&mut self, at: f64, text: String) {
        const KEPT: usize = 8;
        self.banners.push((text, at));
        let over = self.banners.len().saturating_sub(KEPT);
        self.banners.drain(..over);
    }

    /// The words the banner shows for `action`, if it shows any (see the module docs).
    fn action_text(&self, action: ActionRef) -> Option<String> {
        match action {
            ActionRef::Attack => None,
            ActionRef::Defend => Some("Defend".to_string()),
            ActionRef::Magic(id) => Some(spell_name(self.names(), id)),
            ActionRef::Item(id) => Some(item_name(self.names(), id)),
            ActionRef::EnemyAttack(id) => self.names()?.enemy_attack(self.formation, id).map(str::to_string),
            // A limit break is named from the kernel's "Names - magic" table, like a spell (Braver 128, Big
            // Shot 135, Healing Wind 142, Beat Rush 149); `Limit <id>` where this machine's book has no name.
            ActionRef::Limit(id) => Some(limit_name(self.names(), id)),
            ActionRef::Escape => Some("Escape".to_string()),
        }
    }

    /// Battle events arrived at `now`. `my_command` says that one of them is the command of the member the menu is for being
    /// applied.
    pub fn on_events(&mut self, now: f64, events: &[BattleEvent], my_command: bool) {
        self.popups.on_events(now, events);
        self.sounds.on_events(now, events, &self.palette);
        self.dying.retain(|(_, due)| *due > now);
        for cue in cue::cues(events) {
            // Each performance has its banner when it begins (one after the other: a counter's name does not hide the
            // name of the blow it answers before that blow is played).
            let begins = now + cue.start;
            if cue.action.is_some() || cue.length() > 0.0 {
                self.last_start = self.last_start.max(begins);
            }
            if let Some(text) = cue.action.and_then(|(_, action)| self.action_text(action)) {
                self.show_banner(begins, text);
            }
            for event in cue.events {
                match event {
                    BattleEvent::Death { actor } => {
                        self.dying.retain(|(who, _)| who != actor);
                        self.dying.push((*actor, now + cue.delay_of(event)));
                    }
                    // The attempt to run failed: the banner of `Escape` gives way to the verdict (the last one wins).
                    BattleEvent::EscapeFailed { .. } => self.show_banner(begins, "Couldn't escape".to_string()),
                    _ => {}
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

    /// The batch whose events [`BattleHud::on_events`] was told of arrived at `now`, and the engine applied it: `before` and
    /// `after` are everybody's numbers around that (see [`Lag::on_batch`]), so that the panels show them when the blows land.
    pub fn on_vitals(&mut self, now: f64, events: &[BattleEvent], before: &[(ActorId, Vitals)], after: &[(ActorId, Vitals)]) {
        self.lag.on_batch(now, events, before, after);
    }

    /// Dress `view`, the state of a battle that runs, with the live parts of the UI at `now`.
    pub fn decorate(&self, view: &mut BattleView, now: f64) {
        view.popups = self.popups.views(now);
        // The banner on screen is the last one that has begun (a later pushed one wins a tie), while it lasts.
        let mut on_screen: Option<&(String, f64)> = None;
        for banner in self.banners.iter().filter(|(_, at)| *at <= now) {
            if on_screen.is_none_or(|(_, at)| banner.1 >= *at) {
                on_screen = Some(banner);
            }
        }
        view.banner = on_screen.filter(|(_, at)| now - at < BANNER_S).map(|(text, at)| Banner {
            text: text.clone(),
            age_ms: ((now - at).max(0.0) * 1000.0).round() as u32,
        });
        if let Some(mp) = turn_mp(view) {
            let (allies, enemies) = (fighters(&view.party), fighters(&view.enemies));
            view.menu = Some(self.menu.view(&Ctx { model: &self.model, allies: &allies, enemies: &enemies, mp }));
        }
        // After the menu has taken its targets and its MP from the state: the list alone lingers on a corpse, and the
        // panels show the numbers of the blows that have landed.
        self.hold_dying(view, now);
        self.lag.apply(view, now);
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
        self.banners.clear();
        // The end counts from the last performance that begins on screen, when that is later than the message (a counter
        // that wipes the party begins after the blow that provoked it).
        let at = if self.last_start.is_finite() { now.max(self.last_start).min(now + cue::MAX_BATCH_SPAN) } else { now };
        self.ended = Some(Ended { view: last, outcome, reward, at, dismissed: false });
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
        self.lag.apply(&mut view, now);
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
    use crate::battle::sounds::{SoundBook, Why};
    use crate::battle::view::Status;
    use fmv_core::{AccountId, EntityId};
    use fmv_ff7_battle::rules::Target;
    use fmv_protocol::{BattleId, BattleMember};

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
        assert_eq!(hud.press(MenuKey::Confirm, &v), Reply::Send(Choice::Magic { spell: 5, target: Target::One(ActorId(3)) }));
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

    /// Everybody's numbers for [`view`]'s four fighters (party 0 and 1, enemies 2 and 3): `hp` each, out of 10.
    fn everyone(hp: [u32; 4]) -> Vec<(ActorId, Vitals)> {
        hp.iter().enumerate().map(|(i, hp)| (ActorId(i as u8), Vitals { hp: *hp, max_hp: 10, mp: 12, max_mp: 20, alive: *hp > 0, status: fmv_ff7_battle::status::StatusSet::EMPTY })).collect()
    }

    #[test]
    fn holding_a_beaten_enemy_in_the_list_does_not_make_it_a_target_and_a_fallen_party_member_stands_on_the_panel_until_its_blow_lands() {
        let mut hud = hud();
        let batch = [action(1, ActionRef::Attack), BattleEvent::Death { actor: ActorId(3) }, BattleEvent::Death { actor: ActorId(0) }];
        hud.on_events(10.0, &batch, false);
        // The engine's numbers around the batch (what the client takes from the replay): the enemy and party member 0 fall.
        hud.on_vitals(10.0, &batch, &everyone([10, 10, 10, 10]), &everyone([0, 10, 10, 0]));
        // Our turn: Attack, then the target selection.
        let ready = {
            let mut v = view_with_dead_enemy();
            v.party[1].ready = true;
            v.party[0].alive = false;
            v.party[0].hp = 0;
            v
        };
        hud.press(MenuKey::Confirm, &ready);
        let mut dressed = ready.clone();
        hud.decorate(&mut dressed, 10.1);
        let aim = dressed.menu.expect("our menu").aim.expect("the pointer");
        assert_eq!(aim.one(), Some(ActorId(2)), "the pointer is on the living enemy, not on the one still listed");
        assert!(dressed.enemies[1].alive, "though the list still shows it");
        // The party's panel is the engine's state only once the blow that felled the member has landed and its collapse has begun:
        // until then the name is not dead (the menu above was made from the engine's state, which is why it knows better).
        assert!(dressed.party[0].alive, "a fallen party member's name is not dead before its collapse begins");
        let collapse = 10.0 + cue::impact_delay(Some(ActionRef::Attack)) + cue::DEATH_BEAT;
        let mut later = ready.clone();
        hud.decorate(&mut later, collapse - 0.01);
        assert!(later.party[0].alive && later.enemies[1].alive);
        let mut later = ready.clone();
        hud.decorate(&mut later, collapse + 0.01);
        assert!(!later.party[0].alive && !later.enemies[1].alive, "both are down when the collapse begins");
        // With no vitals told (a client with no replay has no numbers to hold back) the panel is the state, at once.
        let mut bare = self::hud();
        bare.on_events(10.0, &batch, false);
        let mut at_once = ready.clone();
        bare.decorate(&mut at_once, 10.1);
        assert!(!at_once.party[0].alive, "without the numbers of a replay there is nothing to wait for");
    }

    #[test]
    fn the_panels_hold_hp_back_until_the_blow_lands_and_the_menu_is_made_from_the_engines_numbers() {
        // Our character (actor 1, ready) drank an Ether in the batch that just arrived: 4 MP became 12 when the item lands. A
        // spell of 8 MP is castable on the engine's 12, and the panel still says 4.
        let mut hud = hud();
        hud.model_mut().spells = vec![MenuSpell::new(5, "Fire", 8, TargetRule::Enemies)];
        let batch = [action(1, ActionRef::Item(3)), BattleEvent::MpRestored { actor: ActorId(1), target: ActorId(1), amount: 8 }];
        hud.on_events(10.0, &batch, true);
        let mut before = everyone([10, 10, 10, 10]);
        before[1].1.mp = 4;
        hud.on_vitals(10.0, &batch, &before, &everyone([10, 10, 10, 10]));
        let lands = 10.0 + cue::impact_delay(Some(ActionRef::Item(3)));
        let ours = view(true);
        hud.press(MenuKey::Down, &ours);
        hud.press(MenuKey::Confirm, &ours);
        let mut dressed = ours.clone();
        hud.decorate(&mut dressed, lands - 0.1);
        assert_eq!(dressed.party[1].mp, 4, "the panel waits for the Ether to land");
        let rows = dressed.menu.expect("our menu").list.expect("the Magic list").rows;
        assert!(rows[0].enabled, "Fire (8 MP) is castable: the menu is made from the engine's 12 MP, not the panel's 4");
        let mut landed = ours.clone();
        hud.decorate(&mut landed, lands + 0.01);
        assert_eq!(landed.party[1].mp, 12);
        // Nobody else's numbers moved.
        assert_eq!((dressed.party[0].hp, dressed.enemies[0].hp, dressed.party[0].mp), (10, 10, 12));
    }

    #[test]
    fn a_counters_damage_is_on_the_panel_when_the_counter_lands_and_the_end_of_the_battle_does_not_cut_it_short() {
        use crate::battle::scene::cue::{impact_delay, length_of};
        let mut hud = hud();
        // A blow on enemy 2 and the counter that answers it, which beats party member 0: the battle ends in one batch.
        let hit = |actor: u8, target: u8, amount: u32| BattleEvent::Damage { actor: ActorId(actor), target: ActorId(target), amount, critical: false };
        let batch = [action(1, ActionRef::Attack), hit(1, 2, 4), action(2, ActionRef::EnemyAttack(272)), hit(2, 0, 10), BattleEvent::Death { actor: ActorId(0) }, BattleEvent::Defeat];
        hud.on_events(20.0, &batch, true);
        let after = everyone([0, 0, 6, 10]);
        hud.on_vitals(20.0, &batch, &everyone([10, 0, 10, 10]), &after);
        let mut last = view(false);
        last.party[0].hp = 0;
        last.party[0].alive = false;
        last.party[1].hp = 0;
        last.party[1].alive = false;
        last.enemies[0].hp = 6;
        hud.finish(20.0, last, Outcome::Defeat, None);
        let counter = 20.0 + length_of(Some(ActionRef::Attack)) + impact_delay(Some(ActionRef::EnemyAttack(272)));
        // The message that ended the battle has long been handled; the panel behind the end panel still has the numbers of
        // the blow, and Cloud's HP only goes when the counter lands.
        let seen = |at: f64| hud.linger_view(at).expect("lingering");
        assert_eq!(seen(20.6).enemies[0].hp, 6, "the blow has landed");
        assert_eq!((seen(20.6).party[0].hp, seen(20.6).party[0].alive), (10, true), "the counter has not");
        assert_eq!((seen(counter + 0.01).party[0].hp, seen(counter + 0.01).party[0].alive), (0, true), "its number is up and the name has not gone dead");
        assert_eq!((seen(counter + 0.4).party[0].hp, seen(counter + 0.4).party[0].alive), (0, false));
    }

    #[test]
    fn a_new_battle_forgets_what_had_not_landed() {
        let mut hud = hud();
        let batch = [action(1, ActionRef::Attack), BattleEvent::Damage { actor: ActorId(1), target: ActorId(2), amount: 4, critical: false }];
        hud.on_events(10.0, &batch, false);
        hud.on_vitals(10.0, &batch, &everyone([10, 10, 10, 10]), &everyone([10, 10, 6, 10]));
        let mut v = view(false);
        v.enemies[0].hp = 6;
        hud.decorate(&mut v, 10.1);
        assert_eq!(v.enemies[0].hp, 10, "held back");
        hud.begin(2, Some(ActorId(1)));
        let mut v = view(false);
        v.enemies[0].hp = 6;
        hud.decorate(&mut v, 10.1);
        assert_eq!(v.enemies[0].hp, 6, "a new battle has nothing in flight");
    }

    /// The party of two players (characters 0 and 3) on a machine whose book knows invented weapons and one attack of scene 75.
    fn sounding_hud() -> BattleHud {
        let book = SoundBook::for_tests().with_weapon(0, 18, 27, 5).with_starting_weapon(0, 0).with_starting_weapon(3, 0).with_scene_attack(75, 272, 15);
        let party = [BattleMember::player(EntityId(1), AccountId(1), "A", 0, 6, 0), BattleMember::player(EntityId(2), AccountId(2), "B", 3, 6, 0)];
        let mut hud = BattleHud::new(None);
        hud.begin(301, Some(ActorId(1)));
        hud.set_palette(crate::battle::sounds::Palette::new(Some(std::sync::Arc::new(book)), 301, &party));
        hud
    }

    #[test]
    fn the_sounds_of_a_batch_are_due_when_its_blows_land_and_stay_due_after_the_battle_ends() {
        let mut hud = sounding_hud();
        let batch = [action(0, ActionRef::Attack), BattleEvent::Damage { actor: ActorId(0), target: ActorId(2), amount: 4, critical: true }];
        hud.on_events(10.0, &batch, false);
        let lands = 10.0 + cue::impact_delay(Some(ActionRef::Attack));
        assert!(hud.sounds_due(0.0, lands - 0.001).is_empty(), "not when the batch arrives");
        let due = hud.sounds_due(lands - 0.001, lands + 0.001);
        assert_eq!(due.iter().map(|(_, s)| (s.id, s.why)).collect::<Vec<_>>(), [(27, Why::Critical)]);
        // The battle ends with that very batch: the sound is still there for the audio to ask for.
        hud.finish(10.0, view(false), Outcome::Victory, None);
        assert_eq!(hud.sounds_due(lands - 0.001, lands + 0.001).len(), 1);
        // The plan the battle can need: the weapon (5, 18, 27), the scene's impact (15) and the two by ear.
        let plan: Vec<(u16, bool)> = hud.sound_plan(&[]).iter().map(|p| (p.id, p.by_ear)).collect();
        assert_eq!(plan, [(5, false), (15, false), (18, false), (27, false), (26, true), (22, true)]);
        // A new battle has none of it.
        hud.begin(1, None);
        assert!(hud.sounds_due(0.0, 100.0).is_empty() && hud.sound_plan(&[]).is_empty());
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
    fn each_action_of_a_batch_has_its_banner_when_it_begins_and_the_last_one_begun_wins() {
        use crate::battle::scene::cue::length_of;
        let mut hud = hud_with_names();
        hud.on_events(5.0, &[action(1, ActionRef::Magic(27)), action(2, ActionRef::EnemyAttack(272)), action(1, ActionRef::Item(7))], false);
        // The actions of a batch are played one after the other: the spell's name is up while the spell is played, the enemy's
        // answer (named, like a counter) comes up when it begins, the item after it, each replacing the one before.
        let second = 5.0 + length_of(Some(ActionRef::Magic(27)));
        let third = second + length_of(Some(ActionRef::EnemyAttack(272)));
        assert_eq!(banner_text(&hud, 5.1).as_deref(), Some("Fire"));
        assert_eq!(banner_text(&hud, second - 0.01).as_deref(), Some("Fire"), "the answer waits for the blow it answers");
        assert_eq!(banner_text(&hud, second + 0.05).as_deref(), Some("Machine Gun"));
        assert_eq!(banner_text(&hud, third + 0.05).as_deref(), Some("Phoenix Down"));
        // A later batch (after that show) replaces it and restarts its time.
        hud.on_events(9.0, &[action(2, ActionRef::EnemyAttack(272))], false);
        let mut v = view(false);
        hud.decorate(&mut v, 9.25);
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

    /// The panel counts from the moment the LAST performance begins on screen: a counter that ends the battle begins after the
    /// blow it answers, so its number must be seen before the panel comes (the panel is due [`PANEL_DELAY_MS`] after that).
    #[test]
    fn the_end_of_the_battle_counts_from_the_last_performance_that_begins_not_from_the_message_that_ended_it() {
        use crate::battle::scene::cue::{impact_delay, length_of};
        let counter_batch = [
            action(1, ActionRef::Attack),
            BattleEvent::Damage { actor: ActorId(1), target: ActorId(2), amount: 40, critical: false },
            action(2, ActionRef::EnemyAttack(272)),
            BattleEvent::Damage { actor: ActorId(2), target: ActorId(1), amount: 999, critical: false },
            BattleEvent::Death { actor: ActorId(1) },
            BattleEvent::Defeat,
        ];
        let mut hud = hud();
        hud.on_events(30.0, &counter_batch, true);
        hud.finish(30.0, view(false), Outcome::Defeat, None);
        let begins = 30.0 + length_of(Some(ActionRef::Attack));
        let age = |t: f64| hud.linger_view(t).and_then(|v| v.finale).map(|f| f.age_ms);
        // Before the counter begins the panel has not started to count; from there it does.
        assert_eq!(age(30.0 + 0.2), Some(0));
        assert_eq!(age(begins), Some(0));
        assert_eq!(age(begins + 0.25), Some(250));
        // The counter's number is up before the panel is due, and the panel comes after it has landed.
        let lands = begins + impact_delay(Some(ActionRef::EnemyAttack(272)));
        let seen = hud.linger_view(lands + 0.01).expect("lingering");
        assert!(seen.popups.iter().any(|p| p.target == ActorId(1) && matches!(p.kind, PopupKind::Damage { amount: 999, .. })), "{:?}", seen.popups);
        assert!(seen.finale.as_ref().is_some_and(|f| f.age_ms < PANEL_DELAY_MS), "the panel is not due yet: {:?}", seen.finale);
        assert!(((lands - begins) * 1000.0) < f64::from(PANEL_DELAY_MS), "it lands before the panel is due");
        // A key right after the message does not skip it: nothing can be closed before the panel has been due and seen.
        assert!(!hud.linger_view(30.2).and_then(|v| v.finale).is_some_and(|f| f.dismissible));
        // A battle whose last batch has ONE performance ends as it did: the panel counts from the message.
        let mut single = self::hud();
        single.on_events(40.0, &[action(1, ActionRef::Attack), BattleEvent::Damage { actor: ActorId(1), target: ActorId(2), amount: 9, critical: false }, BattleEvent::Victory], true);
        single.finish(40.0, view(false), Outcome::Victory, None);
        assert_eq!(single.linger_view(40.1).and_then(|v| v.finale).map(|f| f.age_ms), Some(100));
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
