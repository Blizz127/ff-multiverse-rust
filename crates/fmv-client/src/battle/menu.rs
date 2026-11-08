//! The command menu of our own character: Attack / Magic / Item / Defend, the Magic and Item lists
//! and the pointer that picks a target.
//!
//! A pure state machine over plain data, no GPU and no clock: [`BattleMenu::press`] takes one
//! [`MenuKey`] and what the battle looks like right now ([`Ctx`]) and answers with a [`Reply`];
//! [`BattleMenu::view`] turns the same two things into the [`MenuView`] that the panels draw.
//!
//! # Levels
//!
//! ```text
//! Commands --Confirm on Attack----------------------------> Target --Confirm--> Send(Attack)
//!     |   \--Confirm on Magic--> Magic list --Confirm--------^  (Back returns to where it came from)
//!     |    \-Confirm on Item---> Item list  --Confirm--------^
//!     \--Confirm on Defend------------------------------------------> Send(Defend)
//! ```
//!
//! `Back` goes up one level. At the top level it is [`Reply::Ignored`], which is how the window
//! knows that Esc was not for the menu and falls back to its own handling (which never quits in a
//! battle).
//!
//! # What the menu knows
//!
//! Only what the owner's own data and the engine say, as plain numbers and strings the integrator
//! hands over in a [`BattleMenuModel`]: the spells (id, name, MP cost, whom they aim at), the
//! items (id, name, count, whom they aim at) and whether Defend exists. Until the engine can take
//! more than Attack, the model is empty and Magic, Item and Defend show greyed out.
//!
//! # Targets
//!
//! The target is remembered as an [`ActorId`], never as a position, so it survives the roster
//! changing under the pointer: when the selected fighter dies, the pointer moves to the first valid
//! one left, and when none is left at all the menu steps back a level.

use fmv_ff7_battle::rules::{ActorId, Command};

/// Rows a Magic or Item list shows at once.
pub const LIST_ROWS: usize = 4;

/// The commands, in menu order.
pub const COMMAND_LABELS: [&str; 4] = ["Attack", "Magic", "Item", "Defend"];
const ATTACK: usize = 0;
const MAGIC: usize = 1;
const ITEM: usize = 2;
const DEFEND: usize = 3;

/// A key as the menu understands it. The window maps arrows and WASD to the four directions,
/// Space and Enter to `Confirm`, Backspace and Esc to `Back`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuKey {
    Up,
    Down,
    Left,
    Right,
    Confirm,
    Back,
}

/// Whom something can be aimed at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetRule {
    /// One living enemy (Attack, attack spells).
    Enemies,
    /// One living member of the party (Cure, Potion).
    Allies,
    /// One fallen member of the party (Phoenix Down).
    DeadAllies,
}

impl TargetRule {
    /// The rule of a spell that the engine describes with a plain "aims at allies" flag.
    #[cfg_attr(not(test), allow(dead_code))] // the integrator maps the engine's spell list with it
    pub fn from_allies(allies: bool) -> Self {
        if allies {
            TargetRule::Allies
        } else {
            TargetRule::Enemies
        }
    }

    fn slot(self) -> usize {
        match self {
            TargetRule::Enemies => 0,
            TargetRule::Allies => 1,
            TargetRule::DeadAllies => 2,
        }
    }
}

/// One spell of the Magic list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuSpell {
    pub id: u16,
    pub name: String,
    pub mp_cost: u16,
    /// The engine allows it at all (it is greyed out when `false`; MP is checked on top of this).
    pub enabled: bool,
    pub target: TargetRule,
}

impl MenuSpell {
    #[cfg_attr(not(test), allow(dead_code))] // the integrator builds the list from the engine's spells
    pub fn new(id: u16, name: impl Into<String>, mp_cost: u16, target: TargetRule) -> Self {
        Self { id, name: name.into(), mp_cost, enabled: true, target }
    }
}

/// One entry of the Item list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuItem {
    pub id: u16,
    pub name: String,
    pub count: u16,
    /// The engine allows it at all (greyed out when `false`; the count is checked on top of this).
    pub enabled: bool,
    pub target: TargetRule,
}

impl MenuItem {
    #[cfg_attr(not(test), allow(dead_code))] // the integrator builds the list from the inventory
    pub fn new(id: u16, name: impl Into<String>, count: u16, target: TargetRule) -> Self {
        Self { id, name: name.into(), count, enabled: true, target }
    }

    fn usable(&self) -> bool {
        item_refusal(self).is_none()
    }
}

/// What the menu offers: filled from the engine and the inventory by the integrator, empty before.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BattleMenuModel {
    pub spells: Vec<MenuSpell>,
    pub items: Vec<MenuItem>,
    /// Defend exists (the engine takes the command).
    pub defend: bool,
}

impl BattleMenuModel {
    fn has_magic(&self) -> bool {
        !self.spells.is_empty()
    }

    fn has_items(&self) -> bool {
        self.items.iter().any(MenuItem::usable)
    }
}

/// A fighter as the menu needs to know it: who, and whether it still stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fighter {
    pub id: ActorId,
    pub alive: bool,
}

/// What the menu is told about the battle on every key and every frame.
#[derive(Debug, Clone, Copy)]
pub struct Ctx<'a> {
    pub model: &'a BattleMenuModel,
    /// The party, in display order.
    pub allies: &'a [Fighter],
    /// The enemies, in display order.
    pub enemies: &'a [Fighter],
    /// Our character's MP now.
    pub mp: u32,
}

impl Ctx<'_> {
    /// The fighters `rule` allows, in display order.
    fn candidates(&self, rule: TargetRule) -> Vec<ActorId> {
        let pick = |list: &[Fighter], alive: bool| list.iter().filter(|f| f.alive == alive).map(|f| f.id).collect();
        match rule {
            TargetRule::Enemies => pick(self.enemies, true),
            TargetRule::Allies => pick(self.allies, true),
            TargetRule::DeadAllies => pick(self.allies, false),
        }
    }
}

/// What the player decided: the request the menu ends in. The window turns it into a command for
/// the server ([`Choice::to_command`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Attack { target: ActorId },
    Magic { spell: u16, target: ActorId },
    Item { item: u16, target: ActorId },
    Defend,
}

impl Choice {
    /// The engine command this choice stands for, or `None` for a kind of command this engine
    /// build does not have yet (the window then says so instead of sending something the server
    /// cannot read). When the engine's `Command` gains `Magic`, `Item` and `Defend`, this becomes
    /// a plain one-to-one mapping.
    pub fn to_command(self) -> Option<Command> {
        match self {
            Choice::Attack { target } => Some(Command::Attack { target }),
            Choice::Magic { .. } | Choice::Item { .. } | Choice::Defend => None,
        }
    }

    /// A few words for the log.
    pub fn describe(self, actor: ActorId) -> String {
        match self {
            Choice::Attack { target } => format!("actor {} attacks {}", actor.0, target.0),
            Choice::Magic { spell, target } => format!("actor {} casts spell {spell} on {}", actor.0, target.0),
            Choice::Item { item, target } => format!("actor {} uses item {item} on {}", actor.0, target.0),
            Choice::Defend => format!("actor {} defends", actor.0),
        }
    }
}

/// Why a key could not do what it means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    NoMagic,
    NoItems,
    NoDefend,
    NotEnoughMp,
    OutOfItem,
    Unusable,
    NoTarget,
}

impl Refusal {
    pub fn text(self) -> &'static str {
        match self {
            Refusal::NoMagic => "No magic to cast",
            Refusal::NoItems => "No items to use",
            Refusal::NoDefend => "Not available yet",
            Refusal::NotEnoughMp => "Not enough MP",
            Refusal::OutOfItem => "None left",
            Refusal::Unusable => "Cannot be used now",
            Refusal::NoTarget => "No valid target",
        }
    }
}

/// The menu's answer to a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
    /// The key did something to the menu (moved the cursor, opened or left a level).
    Handled,
    /// The key means nothing here: `Back` at the top level, or a direction on a list that does not
    /// use it. The caller falls back to its own handling.
    Ignored,
    /// The choice is made: send it.
    Send(Choice),
    /// The key was understood but cannot be done.
    Refused(Refusal),
}

/// Where the menu is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Commands,
    Magic,
    Items,
    Target,
}

/// What an open target selection is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Aimed {
    Attack,
    Magic(u16),
    Item(u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Aim {
    what: Aimed,
    rule: TargetRule,
    target: ActorId,
    /// The level `Back` returns to.
    from: Level,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Commands,
    Magic,
    Items,
    Aiming(Aim),
}

impl State {
    fn of_level(level: Level) -> State {
        match level {
            Level::Magic => State::Magic,
            Level::Items => State::Items,
            Level::Commands | Level::Target => State::Commands,
        }
    }
}

/// A cursor in a list with a window of [`LIST_ROWS`] rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Cursor {
    index: usize,
    scroll: usize,
}

impl Cursor {
    /// Keep the cursor inside a list of `len` rows and the window around it.
    fn fit(&mut self, len: usize) {
        self.index = self.index.min(len.saturating_sub(1));
        if self.index < self.scroll {
            self.scroll = self.index;
        } else if self.index >= self.scroll + LIST_ROWS {
            self.scroll = self.index + 1 - LIST_ROWS;
        }
        self.scroll = self.scroll.min(len.saturating_sub(LIST_ROWS));
    }

    /// One row up or down, wrapping round the ends.
    fn step(&mut self, len: usize, down: bool) {
        if len == 0 {
            return;
        }
        self.fit(len);
        self.index = if down { (self.index + 1) % len } else { (self.index + len - 1) % len };
        self.fit(len);
    }

    /// A page up or down, stopping at the ends.
    fn page(&mut self, len: usize, down: bool) {
        self.fit(len);
        self.index = if down { (self.index + LIST_ROWS).min(len.saturating_sub(1)) } else { self.index.saturating_sub(LIST_ROWS) };
        self.fit(len);
    }
}

/// The menu: a cursor position per list and the target selection, nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BattleMenu {
    state: State,
    command: usize,
    magic: Cursor,
    items: Cursor,
    /// The target last chosen for each [`TargetRule`], where the pointer starts next time.
    remembered: [Option<ActorId>; 3],
}

impl Default for BattleMenu {
    fn default() -> Self {
        Self { state: State::Commands, command: ATTACK, magic: Cursor::default(), items: Cursor::default(), remembered: [None; 3] }
    }
}

impl BattleMenu {
    /// Back to the command list (the cursor positions stay: a new turn starts on the command used
    /// last).
    pub fn reset(&mut self) {
        self.state = State::Commands;
    }

    /// The level the menu is at.
    pub fn level(&self) -> Level {
        match self.state {
            State::Commands => Level::Commands,
            State::Magic => Level::Magic,
            State::Items => Level::Items,
            State::Aiming(_) => Level::Target,
        }
    }

    /// The state with a target that is still valid: a target that died is replaced by the first
    /// valid one left, and a selection with nobody left to aim at goes back a level.
    fn resolved(&self, ctx: &Ctx<'_>) -> State {
        let State::Aiming(aim) = self.state else { return self.state };
        let candidates = ctx.candidates(aim.rule);
        if candidates.contains(&aim.target) {
            self.state
        } else if let Some(first) = candidates.first() {
            State::Aiming(Aim { target: *first, ..aim })
        } else {
            State::of_level(aim.from)
        }
    }

    /// One key. See the module docs for what each level does with it.
    pub fn press(&mut self, key: MenuKey, ctx: &Ctx<'_>) -> Reply {
        self.state = self.resolved(ctx);
        match self.state {
            State::Commands => self.on_commands(key, ctx),
            State::Magic => self.on_list(key, ctx, true),
            State::Items => self.on_list(key, ctx, false),
            State::Aiming(aim) => self.on_aim(key, aim, ctx),
        }
    }

    fn on_commands(&mut self, key: MenuKey, ctx: &Ctx<'_>) -> Reply {
        match key {
            MenuKey::Up => {
                self.command = (self.command + COMMAND_LABELS.len() - 1) % COMMAND_LABELS.len();
                Reply::Handled
            }
            MenuKey::Down => {
                self.command = (self.command + 1) % COMMAND_LABELS.len();
                Reply::Handled
            }
            MenuKey::Left | MenuKey::Right | MenuKey::Back => Reply::Ignored,
            MenuKey::Confirm => match self.command {
                ATTACK => self.start_aim(Aimed::Attack, TargetRule::Enemies, Level::Commands, ctx),
                MAGIC if ctx.model.has_magic() => {
                    self.state = State::Magic;
                    Reply::Handled
                }
                MAGIC => Reply::Refused(Refusal::NoMagic),
                ITEM if ctx.model.has_items() => {
                    self.state = State::Items;
                    Reply::Handled
                }
                ITEM => Reply::Refused(Refusal::NoItems),
                DEFEND if ctx.model.defend => Reply::Send(Choice::Defend),
                _ => Reply::Refused(Refusal::NoDefend),
            },
        }
    }

    /// The Magic (`magic`) or the Item list.
    fn on_list(&mut self, key: MenuKey, ctx: &Ctx<'_>, magic: bool) -> Reply {
        let len = if magic { ctx.model.spells.len() } else { ctx.model.items.len() };
        let (cursor, from) = if magic { (&mut self.magic, Level::Magic) } else { (&mut self.items, Level::Items) };
        cursor.fit(len);
        match key {
            MenuKey::Up => cursor.step(len, false),
            MenuKey::Down => cursor.step(len, true),
            MenuKey::Left => cursor.page(len, false),
            MenuKey::Right => cursor.page(len, true),
            MenuKey::Back => {
                self.state = State::Commands;
                return Reply::Handled;
            }
            MenuKey::Confirm => {
                let index = cursor.index;
                let picked = if magic {
                    ctx.model.spells.get(index).map(|s| (Aimed::Magic(s.id), s.target, spell_refusal(s, ctx.mp)))
                } else {
                    ctx.model.items.get(index).map(|i| (Aimed::Item(i.id), i.target, item_refusal(i)))
                };
                return match picked {
                    None => Reply::Refused(if magic { Refusal::NoMagic } else { Refusal::NoItems }),
                    Some((_, _, Some(why))) => Reply::Refused(why),
                    Some((what, rule, None)) => self.start_aim(what, rule, from, ctx),
                };
            }
        }
        Reply::Handled
    }

    /// Open the target selection for `what` on the first valid target (or the one chosen last).
    fn start_aim(&mut self, what: Aimed, rule: TargetRule, from: Level, ctx: &Ctx<'_>) -> Reply {
        let candidates = ctx.candidates(rule);
        let Some(first) = candidates.first().copied() else { return Reply::Refused(Refusal::NoTarget) };
        let target = self.remembered[rule.slot()].filter(|t| candidates.contains(t)).unwrap_or(first);
        self.state = State::Aiming(Aim { what, rule, target, from });
        Reply::Handled
    }

    fn on_aim(&mut self, key: MenuKey, aim: Aim, ctx: &Ctx<'_>) -> Reply {
        let candidates = ctx.candidates(aim.rule);
        let at = candidates.iter().position(|t| *t == aim.target).unwrap_or(0);
        let n = candidates.len().max(1);
        let moved = |at: usize| State::Aiming(Aim { target: candidates.get(at).copied().unwrap_or(aim.target), ..aim });
        match key {
            MenuKey::Up | MenuKey::Left => self.state = moved((at + n - 1) % n),
            MenuKey::Down | MenuKey::Right => self.state = moved((at + 1) % n),
            MenuKey::Back => self.state = State::of_level(aim.from),
            MenuKey::Confirm => {
                self.remembered[aim.rule.slot()] = Some(aim.target);
                self.state = State::Commands;
                let target = aim.target;
                return Reply::Send(match aim.what {
                    Aimed::Attack => Choice::Attack { target },
                    Aimed::Magic(spell) => Choice::Magic { spell, target },
                    Aimed::Item(item) => Choice::Item { item, target },
                });
            }
        }
        Reply::Handled
    }

    /// What to draw.
    pub fn view(&self, ctx: &Ctx<'_>) -> MenuView {
        let state = self.resolved(ctx);
        let commands = [
            CommandRow { label: COMMAND_LABELS[ATTACK], enabled: true },
            CommandRow { label: COMMAND_LABELS[MAGIC], enabled: ctx.model.has_magic() },
            CommandRow { label: COMMAND_LABELS[ITEM], enabled: ctx.model.has_items() },
            CommandRow { label: COMMAND_LABELS[DEFEND], enabled: ctx.model.defend },
        ];
        let mut view = MenuView {
            level: Level::Commands,
            commands,
            command_cursor: self.command.min(COMMAND_LABELS.len() - 1),
            list: None,
            aim: None,
            hint: None,
        };
        match state {
            State::Commands => {
                view.hint = match view.command_cursor {
                    MAGIC if !ctx.model.has_magic() => Some(Refusal::NoMagic.text()),
                    ITEM if !ctx.model.has_items() => Some(Refusal::NoItems.text()),
                    DEFEND if !ctx.model.defend => Some(Refusal::NoDefend.text()),
                    _ => None,
                };
            }
            State::Magic | State::Items => {
                let magic = state == State::Magic;
                view.level = if magic { Level::Magic } else { Level::Items };
                let mut cursor = if magic { self.magic } else { self.items };
                let (rows, hint, total): (Vec<ListRow>, Option<Refusal>, usize) = if magic {
                    let all = &ctx.model.spells;
                    cursor.fit(all.len());
                    let hint = all.get(cursor.index).and_then(|s| spell_refusal(s, ctx.mp));
                    (all.iter().map(|s| spell_row(s, ctx.mp)).collect(), hint, all.len())
                } else {
                    let all = &ctx.model.items;
                    cursor.fit(all.len());
                    let hint = all.get(cursor.index).and_then(item_refusal);
                    (all.iter().map(item_row).collect(), hint, all.len())
                };
                view.hint = hint.map(Refusal::text);
                view.list = Some(ListView {
                    title: if magic { "Magic" } else { "Item" },
                    rows: rows.into_iter().skip(cursor.scroll).take(LIST_ROWS).collect(),
                    cursor: cursor.index - cursor.scroll.min(cursor.index),
                    scroll: cursor.scroll,
                    total,
                });
            }
            State::Aiming(aim) => {
                view.level = Level::Target;
                view.aim = Some(AimView { target: aim.target, enemy: aim.rule == TargetRule::Enemies });
            }
        }
        view
    }
}

/// Why `spell` cannot be cast with `mp` MP, if it cannot.
fn spell_refusal(spell: &MenuSpell, mp: u32) -> Option<Refusal> {
    if !spell.enabled {
        Some(Refusal::Unusable)
    } else if u32::from(spell.mp_cost) > mp {
        Some(Refusal::NotEnoughMp)
    } else {
        None
    }
}

/// Why `item` cannot be used, if it cannot.
fn item_refusal(item: &MenuItem) -> Option<Refusal> {
    if !item.enabled {
        Some(Refusal::Unusable)
    } else if item.count == 0 {
        Some(Refusal::OutOfItem)
    } else {
        None
    }
}

fn spell_row(s: &MenuSpell, mp: u32) -> ListRow {
    ListRow { label: s.name.clone(), right: format!("MP {}", s.mp_cost), enabled: spell_refusal(s, mp).is_none() }
}

fn item_row(i: &MenuItem) -> ListRow {
    ListRow { label: i.name.clone(), right: format!("x{}", i.count), enabled: item_refusal(i).is_none() }
}

/// One command of the first list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandRow {
    pub label: &'static str,
    pub enabled: bool,
}

/// One row of the Magic or Item list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListRow {
    pub label: String,
    /// The right-hand column: `MP 12` or `x3`.
    pub right: String,
    pub enabled: bool,
}

/// The window of rows a Magic or Item list shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListView {
    pub title: &'static str,
    /// The visible rows, at most [`LIST_ROWS`].
    pub rows: Vec<ListRow>,
    /// The cursor's row within `rows`.
    pub cursor: usize,
    /// The index of the first visible row in the whole list.
    pub scroll: usize,
    /// Rows in the whole list.
    pub total: usize,
}

/// The pointer of an open target selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AimView {
    pub target: ActorId,
    /// The target is on the enemy side.
    pub enemy: bool,
}

/// Everything the panels draw of the menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuView {
    pub level: Level,
    pub commands: [CommandRow; 4],
    pub command_cursor: usize,
    /// The Magic or Item list, at those levels.
    pub list: Option<ListView>,
    /// The target pointer, at the target level.
    pub aim: Option<AimView>,
    /// A few words about why the entry under the cursor cannot be chosen.
    pub hint: Option<&'static str>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fighters(alive: &[bool], first: u8) -> Vec<Fighter> {
        alive.iter().enumerate().map(|(i, a)| Fighter { id: ActorId(first + i as u8), alive: *a }).collect()
    }

    /// A model with two attack spells, a healing one, two potions and a Phoenix-Down-like item.
    fn model() -> BattleMenuModel {
        BattleMenuModel {
            spells: vec![
                MenuSpell::new(1, "Fire", 4, TargetRule::Enemies),
                MenuSpell::new(2, "Ice", 4, TargetRule::Enemies),
                MenuSpell::new(3, "Cure", 5, TargetRule::from_allies(true)),
            ],
            items: vec![
                MenuItem::new(10, "Potion", 3, TargetRule::Allies),
                MenuItem::new(11, "Phoenix Down", 2, TargetRule::DeadAllies),
                MenuItem::new(12, "Empty Vial", 0, TargetRule::Allies),
            ],
            defend: true,
        }
    }

    /// Three allies (actors 0..3, all alive) and three enemies (actors 3..6, all alive).
    struct Scene {
        model: BattleMenuModel,
        allies: Vec<Fighter>,
        enemies: Vec<Fighter>,
        mp: u32,
    }

    impl Scene {
        fn new() -> Self {
            Self { model: model(), allies: fighters(&[true; 3], 0), enemies: fighters(&[true; 3], 3), mp: 20 }
        }

        fn ctx(&self) -> Ctx<'_> {
            Ctx { model: &self.model, allies: &self.allies, enemies: &self.enemies, mp: self.mp }
        }
    }

    fn press(m: &mut BattleMenu, s: &Scene, keys: &[MenuKey]) -> Reply {
        let mut last = Reply::Ignored;
        for k in keys {
            last = m.press(*k, &s.ctx());
        }
        last
    }

    use MenuKey::{Back, Confirm, Down, Left, Right, Up};

    #[test]
    fn the_command_list_wraps_and_attack_goes_to_the_first_living_enemy() {
        let s = Scene::new();
        let mut m = BattleMenu::default();
        assert_eq!((m.level(), m.view(&s.ctx()).command_cursor), (Level::Commands, 0));
        assert_eq!(press(&mut m, &s, &[Up]), Reply::Handled);
        assert_eq!(m.view(&s.ctx()).command_cursor, 3, "up from the top wraps to Defend");
        assert_eq!(press(&mut m, &s, &[Down]), Reply::Handled);
        assert_eq!(m.view(&s.ctx()).command_cursor, 0, "down from the bottom wraps to Attack");
        // Enter on Attack opens the target selection on the first enemy, a second Enter attacks it.
        assert_eq!(press(&mut m, &s, &[Confirm]), Reply::Handled);
        assert_eq!(m.level(), Level::Target);
        let aim = m.view(&s.ctx()).aim.expect("a pointer");
        assert_eq!((aim.target, aim.enemy), (ActorId(3), true));
        assert_eq!(press(&mut m, &s, &[Confirm]), Reply::Send(Choice::Attack { target: ActorId(3) }));
        assert_eq!(m.level(), Level::Commands, "the menu is back at the top once a choice is made");
    }

    #[test]
    fn target_cycling_wraps_skips_the_dead_and_remembers_the_last_choice() {
        let mut s = Scene::new();
        s.enemies[1].alive = false; // actor 4 is dead
        let mut m = BattleMenu::default();
        press(&mut m, &s, &[Confirm]);
        let at = |m: &BattleMenu, s: &Scene| m.view(&s.ctx()).aim.expect("aim").target.0;
        assert_eq!(at(&m, &s), 3);
        press(&mut m, &s, &[Right]);
        assert_eq!(at(&m, &s), 5, "the dead enemy is skipped");
        press(&mut m, &s, &[Down]);
        assert_eq!(at(&m, &s), 3, "wraps round to the first");
        press(&mut m, &s, &[Left]);
        assert_eq!(at(&m, &s), 5, "left goes the other way, wrapping too");
        press(&mut m, &s, &[Up]);
        assert_eq!(at(&m, &s), 3);
        // Choose the last one; the next attack starts the pointer there.
        press(&mut m, &s, &[Left]);
        assert_eq!(press(&mut m, &s, &[Confirm]), Reply::Send(Choice::Attack { target: ActorId(5) }));
        press(&mut m, &s, &[Confirm]);
        assert_eq!(at(&m, &s), 5, "the pointer starts where it was last");
        // A remembered target that has died is not offered any more.
        s.enemies[2].alive = false;
        press(&mut m, &s, &[Back]);
        press(&mut m, &s, &[Confirm]);
        assert_eq!(at(&m, &s), 3);
    }

    #[test]
    fn a_single_candidate_stays_put_and_nobody_to_aim_at_refuses() {
        let mut s = Scene::new();
        s.enemies = fighters(&[false, true, false], 3);
        let mut m = BattleMenu::default();
        press(&mut m, &s, &[Confirm]);
        for k in [Up, Down, Left, Right] {
            press(&mut m, &s, &[k]);
            assert_eq!(m.view(&s.ctx()).aim.expect("aim").target, ActorId(4));
        }
        // No living enemy at all: Attack cannot even open the selection.
        let mut m = BattleMenu::default();
        s.enemies = fighters(&[false; 3], 3);
        assert_eq!(press(&mut m, &s, &[Confirm]), Reply::Refused(Refusal::NoTarget));
        assert_eq!(m.level(), Level::Commands);
    }

    #[test]
    fn magic_lists_spells_with_their_cost_and_greys_out_what_mp_cannot_pay() {
        let mut s = Scene::new();
        s.mp = 4;
        let mut m = BattleMenu::default();
        assert_eq!(press(&mut m, &s, &[Down, Confirm]), Reply::Handled);
        assert_eq!(m.level(), Level::Magic);
        let v = m.view(&s.ctx());
        let list = v.list.expect("the list");
        assert_eq!(list.title, "Magic");
        let rows: Vec<(&str, &str, bool)> = list.rows.iter().map(|r| (r.label.as_str(), r.right.as_str(), r.enabled)).collect();
        assert_eq!(rows, [("Fire", "MP 4", true), ("Ice", "MP 4", true), ("Cure", "MP 5", false)]);
        assert_eq!(v.hint, None, "the cursor is on an affordable spell");
        // The greyed-out one says why and cannot be chosen.
        press(&mut m, &s, &[Down, Down]);
        assert_eq!(m.view(&s.ctx()).hint, Some("Not enough MP"));
        assert_eq!(press(&mut m, &s, &[Confirm]), Reply::Refused(Refusal::NotEnoughMp));
        assert_eq!(m.level(), Level::Magic, "a refusal changes nothing");
        // With enough MP it opens the selection among ALLIES and sends the spell.
        s.mp = 5;
        assert_eq!(press(&mut m, &s, &[Confirm]), Reply::Handled);
        let aim = m.view(&s.ctx()).aim.expect("aim");
        assert_eq!((aim.target, aim.enemy), (ActorId(0), false), "Cure aims at the party");
        press(&mut m, &s, &[Right]);
        assert_eq!(press(&mut m, &s, &[Confirm]), Reply::Send(Choice::Magic { spell: 3, target: ActorId(1) }));
    }

    #[test]
    fn an_attack_spell_aims_at_enemies_and_back_returns_to_the_list_with_the_cursor_kept() {
        let s = Scene::new();
        let mut m = BattleMenu::default();
        press(&mut m, &s, &[Down, Confirm, Down, Confirm]);
        assert_eq!(m.level(), Level::Target);
        let aim = m.view(&s.ctx()).aim.expect("aim");
        assert_eq!((aim.target, aim.enemy), (ActorId(3), true));
        assert_eq!(press(&mut m, &s, &[Back]), Reply::Handled);
        assert_eq!(m.level(), Level::Magic);
        assert_eq!(m.view(&s.ctx()).list.expect("list").cursor, 1, "still on Ice");
        assert_eq!(press(&mut m, &s, &[Back]), Reply::Handled);
        assert_eq!(m.level(), Level::Commands);
        assert_eq!(m.view(&s.ctx()).command_cursor, 1, "still on Magic");
        // At the top there is nothing left to go back to: the key is not the menu's.
        assert_eq!(press(&mut m, &s, &[Back]), Reply::Ignored);
        assert_eq!(m.level(), Level::Commands);
    }

    #[test]
    fn items_show_counts_and_phoenix_down_only_aims_at_the_fallen() {
        let mut s = Scene::new();
        let mut m = BattleMenu::default();
        press(&mut m, &s, &[Down, Down, Confirm]);
        assert_eq!(m.level(), Level::Items);
        let list = m.view(&s.ctx()).list.expect("list");
        assert_eq!(list.title, "Item");
        let rows: Vec<(&str, &str, bool)> = list.rows.iter().map(|r| (r.label.as_str(), r.right.as_str(), r.enabled)).collect();
        assert_eq!(rows, [("Potion", "x3", true), ("Phoenix Down", "x2", true), ("Empty Vial", "x0", false)]);
        // Nobody has fallen: Phoenix Down has no one to aim at.
        press(&mut m, &s, &[Down]);
        assert_eq!(press(&mut m, &s, &[Confirm]), Reply::Refused(Refusal::NoTarget));
        assert_eq!(m.level(), Level::Items);
        // Somebody fell: only the fallen is offered, whatever is pressed.
        s.allies[2].alive = false;
        assert_eq!(press(&mut m, &s, &[Confirm]), Reply::Handled);
        for k in [Right, Left, Down, Up] {
            press(&mut m, &s, &[k]);
            let aim = m.view(&s.ctx()).aim.expect("aim");
            assert_eq!((aim.target, aim.enemy), (ActorId(2), false));
        }
        assert_eq!(press(&mut m, &s, &[Confirm]), Reply::Send(Choice::Item { item: 11, target: ActorId(2) }));
        // A potion aims at the living, and the empty vial cannot be picked.
        let mut m = BattleMenu::default();
        press(&mut m, &s, &[Down, Down, Confirm, Confirm]);
        let aim = m.view(&s.ctx()).aim.expect("aim");
        assert_eq!(aim.target, ActorId(0));
        press(&mut m, &s, &[Right, Right]);
        assert_eq!(m.view(&s.ctx()).aim.expect("aim").target, ActorId(0), "only two are alive, so two steps come full circle");
        press(&mut m, &s, &[Back, Down, Down]);
        assert_eq!(m.view(&s.ctx()).hint, Some("None left"));
        assert_eq!(press(&mut m, &s, &[Confirm]), Reply::Refused(Refusal::OutOfItem));
    }

    #[test]
    fn what_the_model_lacks_is_greyed_out_and_cannot_be_opened() {
        let mut s = Scene::new();
        s.model = BattleMenuModel::default();
        let mut m = BattleMenu::default();
        let v = m.view(&s.ctx());
        let flags: Vec<(&str, bool)> = v.commands.iter().map(|c| (c.label, c.enabled)).collect();
        assert_eq!(flags, [("Attack", true), ("Magic", false), ("Item", false), ("Defend", false)]);
        assert_eq!(press(&mut m, &s, &[Down, Confirm]), Reply::Refused(Refusal::NoMagic));
        assert_eq!(m.view(&s.ctx()).hint, Some("No magic to cast"));
        assert_eq!(press(&mut m, &s, &[Down, Confirm]), Reply::Refused(Refusal::NoItems));
        assert_eq!(press(&mut m, &s, &[Down, Confirm]), Reply::Refused(Refusal::NoDefend));
        assert_eq!(m.level(), Level::Commands);
        // Attack still works.
        assert_eq!(press(&mut m, &s, &[Down, Confirm]), Reply::Handled);
        assert_eq!(m.level(), Level::Target);
    }

    #[test]
    fn defend_sends_at_once_when_the_engine_has_it() {
        let s = Scene::new();
        let mut m = BattleMenu::default();
        assert_eq!(press(&mut m, &s, &[Up, Confirm]), Reply::Send(Choice::Defend));
        assert_eq!(m.level(), Level::Commands);
    }

    #[test]
    fn a_long_list_scrolls_with_the_cursor_wraps_and_pages() {
        let mut s = Scene::new();
        s.model.spells = (0..10).map(|i| MenuSpell::new(i, format!("Spell {i}"), 1, TargetRule::Enemies)).collect();
        let mut m = BattleMenu::default();
        press(&mut m, &s, &[Down, Confirm]);
        let view = |m: &BattleMenu| m.view(&s.ctx()).list.expect("list");
        let first = |m: &BattleMenu| view(m).rows[0].label.clone();
        assert_eq!((view(&m).total, view(&m).rows.len(), view(&m).scroll), (10, LIST_ROWS, 0));
        for _ in 0..3 {
            press(&mut m, &s, &[Down]);
        }
        assert_eq!((view(&m).cursor, view(&m).scroll), (3, 0), "still inside the first window");
        press(&mut m, &s, &[Down]);
        assert_eq!((view(&m).cursor, view(&m).scroll, first(&m)), (3, 1, "Spell 1".to_string()), "the window follows the cursor");
        // Up from the top wraps to the bottom, and the window shows the last rows.
        let mut m = BattleMenu::default();
        press(&mut m, &s, &[Down, Confirm, Up]);
        assert_eq!((view(&m).scroll, first(&m), view(&m).cursor), (6, "Spell 6".to_string(), 3));
        press(&mut m, &s, &[Down]);
        assert_eq!((view(&m).scroll, view(&m).cursor), (0, 0), "down from the bottom wraps to the top");
        // Pages: Right moves a window down, Left up, and they stop at the ends.
        press(&mut m, &s, &[Right]);
        assert_eq!(view(&m).scroll + view(&m).cursor, 4);
        press(&mut m, &s, &[Right, Right, Right]);
        assert_eq!(view(&m).scroll + view(&m).cursor, 9);
        press(&mut m, &s, &[Left, Left, Left, Left, Left]);
        assert_eq!(view(&m).scroll + view(&m).cursor, 0);
        // The picked row is the one under the cursor, not the first on screen.
        let mut m = BattleMenu::default();
        press(&mut m, &s, &[Down, Confirm, Right, Down, Confirm]);
        assert_eq!(press(&mut m, &s, &[Confirm]), Reply::Send(Choice::Magic { spell: 5, target: ActorId(3) }));
    }

    #[test]
    fn a_target_that_dies_under_the_pointer_is_replaced_and_a_wiped_out_side_goes_back_a_level() {
        let mut s = Scene::new();
        let mut m = BattleMenu::default();
        press(&mut m, &s, &[Down, Confirm, Confirm]);
        assert_eq!(m.view(&s.ctx()).aim.expect("aim").target, ActorId(3));
        // The aimed-at enemy dies while the pointer is on it: the view and the next key agree on the
        // replacement.
        s.enemies[0].alive = false;
        assert_eq!(m.view(&s.ctx()).aim.expect("aim").target, ActorId(4));
        assert_eq!(press(&mut m, &s, &[Confirm]), Reply::Send(Choice::Magic { spell: 1, target: ActorId(4) }));
        // Everybody dies: the selection is gone and the menu is back on the list it came from.
        let mut m = BattleMenu::default();
        s.enemies = fighters(&[true; 3], 3);
        press(&mut m, &s, &[Down, Confirm, Confirm]);
        s.enemies = fighters(&[false; 3], 3);
        let v = m.view(&s.ctx());
        assert_eq!((v.level, v.aim), (Level::Magic, None));
        assert_eq!(press(&mut m, &s, &[Back]), Reply::Handled, "that Back leaves the list, not a selection that no longer exists");
        assert_eq!(m.level(), Level::Commands);
    }

    #[test]
    fn the_list_shrinking_under_the_cursor_never_panics() {
        let mut s = Scene::new();
        s.model.spells = (0..9).map(|i| MenuSpell::new(i, format!("S{i}"), 1, TargetRule::Enemies)).collect();
        let mut m = BattleMenu::default();
        press(&mut m, &s, &[Down, Confirm, Up, Up]);
        s.model.spells.truncate(2);
        let list = m.view(&s.ctx()).list.expect("list");
        assert!(list.cursor < list.rows.len() && list.scroll == 0);
        s.model.spells.clear();
        let v = m.view(&s.ctx());
        assert!(v.list.expect("list").rows.is_empty());
        assert_eq!(press(&mut m, &s, &[Confirm]), Reply::Refused(Refusal::NoMagic));
        for k in [Up, Down, Left, Right, Confirm, Back] {
            press(&mut m, &s, &[k]);
        }
    }

    #[test]
    fn reset_returns_to_the_commands_and_keeps_the_cursor_on_the_last_command() {
        let s = Scene::new();
        let mut m = BattleMenu::default();
        press(&mut m, &s, &[Down, Down, Confirm]);
        assert_eq!(m.level(), Level::Items);
        m.reset();
        assert_eq!(m.level(), Level::Commands);
        assert_eq!(m.view(&s.ctx()).command_cursor, 2);
    }

    #[test]
    fn choices_become_engine_commands_where_the_engine_has_them() {
        assert_eq!(Choice::Attack { target: ActorId(4) }.to_command(), Some(Command::Attack { target: ActorId(4) }));
        for other in [Choice::Magic { spell: 1, target: ActorId(0) }, Choice::Item { item: 1, target: ActorId(0) }, Choice::Defend] {
            assert_eq!(other.to_command(), None, "{other:?} is not in this engine build yet");
        }
        assert!(Choice::Defend.describe(ActorId(1)).contains("defends"));
        assert!(Choice::Magic { spell: 7, target: ActorId(3) }.describe(ActorId(1)).contains("spell 7"));
    }

    /// Whatever is pressed in whatever order, the menu never panics, never leaves a level that does
    /// not exist, and only ever sends a choice whose target the rule allows.
    #[test]
    fn random_keys_over_changing_rosters_stay_sane() {
        use crate::ui::testkit::Rng;
        let mut rng = Rng::new(0xB477_1E);
        let keys = [Up, Down, Left, Right, Confirm, Back];
        for round in 0..200 {
            let mut s = Scene::new();
            let mut m = BattleMenu::default();
            for step in 0..120 {
                if rng.chance(6) {
                    let n = rng.below(5);
                    s.enemies = fighters(&(0..n).map(|_| rng.chance(2)).collect::<Vec<_>>(), 10);
                    let n = rng.below(5);
                    s.allies = fighters(&(0..n).map(|_| rng.chance(2)).collect::<Vec<_>>(), 0);
                    s.mp = rng.below(8) as u32;
                    if rng.chance(3) {
                        s.model.spells.truncate(rng.below(4));
                        s.model.items.truncate(rng.below(4));
                    }
                }
                let reply = m.press(keys[rng.below(keys.len())], &s.ctx());
                if let Reply::Send(choice) = reply {
                    let (target, rule) = match choice {
                        Choice::Attack { target } => (Some(target), TargetRule::Enemies),
                        Choice::Magic { spell, target } => (Some(target), s.model.spells.iter().find(|x| x.id == spell).map_or(TargetRule::Enemies, |x| x.target)),
                        Choice::Item { item, target } => (Some(target), s.model.items.iter().find(|x| x.id == item).map_or(TargetRule::Allies, |x| x.target)),
                        Choice::Defend => (None, TargetRule::Enemies),
                    };
                    if let Some(t) = target {
                        assert!(s.ctx().candidates(rule).contains(&t), "round {round} step {step}: {choice:?} aims at a fighter the rule does not allow");
                    }
                }
                let v = m.view(&s.ctx());
                if let Some(list) = &v.list {
                    assert!(list.rows.len() <= LIST_ROWS && (list.rows.is_empty() || list.cursor < list.rows.len()), "round {round} step {step}");
                }
                assert_eq!(v.aim.is_some(), v.level == Level::Target);
            }
        }
    }
}
