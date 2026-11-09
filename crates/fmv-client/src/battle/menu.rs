//! The command menu of our own character: Attack / Magic / Item / Defend / Escape, the Magic and Item
//! lists and the pointer that picks a target.
//!
//! A pure state machine over plain data, no GPU and no clock: [`BattleMenu::press`] takes one
//! [`MenuKey`] and what the battle looks like right now ([`Ctx`]) and answers with a [`Reply`];
//! [`BattleMenu::view`] turns the same two things into the [`MenuView`] that the panels draw.
//!
//! # Levels
//!
//! ```text
//! Commands --Confirm on Attack (or Limit)-----------------> Target --Confirm--> Send(Attack or Limit)
//!     |   \--Confirm on Magic--> Magic list --Confirm--------^  (Back returns to where it came from)
//!     |    \-Confirm on Item---> Item list  --Confirm--------^
//!     |--Confirm on Defend------------------------------------------> Send(Defend)
//!     \--Confirm on Escape------------------------------------------> Send(Escape)
//! ```
//!
//! **Limit** (protocol v7) takes the place of Attack, as in FF7, when our character's limit gauge is full:
//! the first row of the list says `Limit` instead of `Attack` and opens the target selection among the
//! limit's own targets (the enemies, or the party for a healing limit like Healing Wind). **Escape** is the
//! last row (FF7 runs by holding L1+R1, which a window that only has keys cannot do cleanly: running is
//! the actor's turn here, see `fmv_ff7_battle::rules`, "Escape"); it sends at once, and is greyed out when
//! the battle cannot be run from.
//!
//! `Back` goes up one level. At the top level it is [`Reply::Ignored`], which is how the window
//! knows that Esc was not for the menu and falls back to its own handling (which never quits in a
//! battle).
//!
//! # What the menu knows
//!
//! Only what the owner's own data and the engine say, as plain numbers and strings the client
//! hands over in a [`BattleMenuModel`] (it fills it from the replayed engine and the inventory the
//! server sent, see `BattleClient::refresh_menu_model`): the spells (id, name, MP cost, whom they
//! aim at), the items (id, name, count, whom they aim at), whether Defend exists, our character's limit
//! (id, name, whom it aims at, whether its gauge is full) and whether the battle can be escaped. What the
//! model lacks is greyed out and cannot be opened: an empty model (this machine has no battle data, or we
//! are not in the party) leaves only Attack working.
//!
//! # Targets
//!
//! The target is remembered as an [`ActorId`], never as a position, so it survives the roster
//! changing under the pointer: when the selected fighter dies, the pointer moves to the first valid
//! one left, and when none is left at all the menu steps back a level.
//!
//! # Whole sides (protocol v10)
//!
//! A spell, an item or a limit that can hit a whole side ([`Reach`]) has one more place for the pointer:
//! **ALL**, which brackets every fighter of the side at once (the engine says who is on it: the living, or the
//! fallen for a revival) and sends `Target::AllEnemies` or `Target::AllAllies`. There is no key for it, so a
//! window that only has the arrows and Enter needs nothing new: the pointer starts on the first target as it
//! always did, and **Down or Right past the last target (or Up or Left before the first) moves it to ALL**,
//! and one more step goes back round to the first target. What can be aimed that way is the engine's word,
//! from this machine's own data, filled into the model at every message:
//!
//! * [`Reach::OneOrAll`]: Cure and Fire and every spell of their kind have the ALL place only while the
//!   character has an All materia use left this battle ([`BattleMenuModel::all_left`]; the list says
//!   `ALL x1`), because that is FF7's rule for them; an item or a limit of that kind always has it;
//! * [`Reach::All`]: Ultima, Megalixir and Healing Wind hit a whole side and nothing else, so the pointer starts
//!   on ALL and stays there (the arrows do nothing) until Enter sends it or Backspace goes back;
//! * [`Reach::One`]: everything else, as before.
//!
//! # Silence
//!
//! A silenced character cannot cast: the engine refuses the Magic command (`CommandError::Silenced`), so the menu
//! does not offer it ([`BattleMenuModel::silenced`], filled from the replayed engine at every message): the Magic
//! row is greyed out and says why under the cursor ([`Refusal::Silenced`]), Enter on it does nothing, and a Magic
//! list (or a target selection for a spell) that is open when the voice goes closes back to the commands.

use fmv_ff7_battle::rules::{ActorId, Command, Target};

/// Rows a Magic or Item list shows at once.
pub const LIST_ROWS: usize = 4;

/// The commands, in menu order.
pub const COMMAND_LABELS: [&str; 5] = ["Attack", "Magic", "Item", "Defend", "Escape"];
/// What the first command says instead of "Attack" when the limit gauge is full.
pub const LIMIT_LABEL: &str = "Limit";
const ATTACK: usize = 0;
const MAGIC: usize = 1;
const ITEM: usize = 2;
const DEFEND: usize = 3;
const ESCAPE: usize = 4;

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

/// How many a spell, item or limit can be aimed at, as the menu offers it (the engine's `Aim`, with the All
/// materia folded in by the client: see the module docs, "Whole sides").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Reach {
    /// One target.
    #[default]
    One,
    /// One target, or the whole side (the pointer has an ALL place).
    OneOrAll,
    /// The whole side only (the pointer starts on ALL and stays).
    All,
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
    /// One target, or a whole side too, or a whole side only (see [`Reach`]).
    pub reach: Reach,
}

impl MenuSpell {
    pub fn new(id: u16, name: impl Into<String>, mp_cost: u16, target: TargetRule) -> Self {
        Self { id, name: name.into(), mp_cost, enabled: true, target, reach: Reach::One }
    }

    /// The same spell with a way of aiming it that is not one target.
    pub fn with_reach(self, reach: Reach) -> Self {
        Self { reach, ..self }
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
    /// One target, or a whole side too, or a whole side only (see [`Reach`]).
    pub reach: Reach,
}

impl MenuItem {
    pub fn new(id: u16, name: impl Into<String>, count: u16, target: TargetRule) -> Self {
        Self { id, name: name.into(), count, enabled: true, target, reach: Reach::One }
    }

    /// The same item with a way of aiming it that is not one target.
    pub fn with_reach(self, reach: Reach) -> Self {
        Self { reach, ..self }
    }

    fn usable(&self) -> bool {
        item_refusal(self).is_none()
    }
}

/// Our character's limit break (protocol v7), as the menu needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuLimit {
    /// The limit's id (what `ActionRef::Limit` carries).
    pub id: u16,
    /// Its name from this machine's own text (Braver, Big Shot, ...).
    pub name: String,
    /// Whom it aims at: the enemies, or the party for a healing limit.
    pub target: TargetRule,
    /// The gauge is full: the first command says `Limit` instead of `Attack`. The client refreshes it with
    /// every message of the battle (the gauge moves only with them).
    pub ready: bool,
    /// One target, or the whole party only (Healing Wind: [`Reach::All`]).
    pub reach: Reach,
}

impl MenuLimit {
    pub fn new(id: u16, name: impl Into<String>, target: TargetRule, ready: bool) -> Self {
        Self { id, name: name.into(), target, ready, reach: Reach::One }
    }

    /// The same limit with a way of aiming it that is not one target.
    pub fn with_reach(self, reach: Reach) -> Self {
        Self { reach, ..self }
    }
}

/// What the menu offers: filled from the engine and the inventory at every battle start, every
/// inventory message and every message of the battle (a limit gauge moves with them), empty (only Attack)
/// while there is nothing to fill it from.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BattleMenuModel {
    pub spells: Vec<MenuSpell>,
    pub items: Vec<MenuItem>,
    /// Defend exists (the engine takes the command).
    pub defend: bool,
    /// Our character's limit break, if it has one.
    pub limit: Option<MenuLimit>,
    /// The battle can be run from (the engine takes `Escape`).
    pub escape: bool,
    /// How many more times in this battle our character may hit a whole side with a spell that needs an All
    /// materia use (the list shows it; a spell that needs one has [`Reach::OneOrAll`] only while this is not 0,
    /// which the client sets when it fills the model).
    pub all_left: u8,
    /// Our character is silenced (the engine refuses its Magic command): the Magic row is greyed out.
    pub silenced: bool,
}

impl BattleMenuModel {
    fn has_magic(&self) -> bool {
        !self.spells.is_empty()
    }

    /// Magic can be opened: there is something to cast and the voice is there.
    fn can_cast(&self) -> bool {
        self.has_magic() && !self.silenced
    }

    fn has_items(&self) -> bool {
        self.items.iter().any(MenuItem::usable)
    }

    /// The limit our character can give now: it has one and its gauge is full.
    fn ready_limit(&self) -> Option<&MenuLimit> {
        self.limit.as_ref().filter(|l| l.ready)
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
    /// A spell on one target or a whole side (protocol v10).
    Magic { spell: u16, target: Target },
    /// An item on one target or a whole side (protocol v10).
    Item { item: u16, target: Target },
    Defend,
    /// Our character's limit break (protocol v7), on one target or the whole party (v10).
    Limit { target: Target },
    /// Run (protocol v7).
    Escape,
}

impl Choice {
    /// The engine command this choice stands for: a plain one-to-one mapping (the server and the
    /// engine decide whether the spell, item and target are valid).
    pub fn to_command(self) -> Command {
        match self {
            Choice::Attack { target } => Command::Attack { target },
            Choice::Magic { spell, target } => Command::Magic { spell, target },
            Choice::Item { item, target } => Command::Item { item, target },
            Choice::Defend => Command::Defend,
            Choice::Limit { target } => Command::Limit { target },
            Choice::Escape => Command::Escape,
        }
    }
}

/// Why a key could not do what it means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    NoMagic,
    /// The character is silenced.
    Silenced,
    NoItems,
    NoDefend,
    /// The battle cannot be run from (a boss, or no battle data here to say it can).
    NoEscape,
    NotEnoughMp,
    OutOfItem,
    Unusable,
    NoTarget,
}

impl Refusal {
    pub fn text(self) -> &'static str {
        match self {
            Refusal::NoMagic => "No magic to cast",
            Refusal::Silenced => "Silenced: cannot cast",
            Refusal::NoItems => "No items to use",
            Refusal::NoDefend => "Not available yet",
            Refusal::NoEscape => "Cannot escape from this battle",
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
    Limit,
}

/// Where the pointer of an open target selection is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum At {
    /// On this one fighter.
    One(ActorId),
    /// On the whole side (every fighter [`Ctx::candidates`] says).
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Aiming {
    what: Aimed,
    rule: TargetRule,
    /// What the pointer may be on besides one fighter.
    reach: Reach,
    at: At,
    /// The level `Back` returns to.
    from: Level,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Commands,
    Magic,
    Items,
    Aiming(Aiming),
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
    #[cfg(test)]
    pub fn level(&self) -> Level {
        match self.state {
            State::Commands => Level::Commands,
            State::Magic => Level::Magic,
            State::Items => Level::Items,
            State::Aiming(_) => Level::Target,
        }
    }

    /// The state with a target that is still valid: a target that died is replaced by the first
    /// valid one left, and a selection with nobody left to aim at goes back a level. The whole side stays
    /// the whole side for as long as anybody is on it.
    fn resolved(&self, ctx: &Ctx<'_>) -> State {
        // The voice is gone (Silence): no Magic list, and no target for a spell, goes on being open.
        if ctx.model.silenced {
            match self.state {
                State::Magic => return State::Commands,
                State::Aiming(Aiming { what: Aimed::Magic(_), .. }) => return State::Commands,
                _ => {}
            }
        }
        let State::Aiming(aim) = self.state else { return self.state };
        let candidates = ctx.candidates(aim.rule);
        match aim.at {
            At::All if !candidates.is_empty() => self.state,
            At::One(target) if candidates.contains(&target) => self.state,
            At::One(_) => match candidates.first() {
                Some(first) => State::Aiming(Aiming { at: At::One(*first), ..aim }),
                None => State::of_level(aim.from),
            },
            At::All => State::of_level(aim.from),
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
                // A full limit gauge puts the limit where Attack was.
                ATTACK => match ctx.model.ready_limit() {
                    Some(limit) => self.start_aim(Aimed::Limit, limit.target, limit.reach, Level::Commands, ctx),
                    None => self.start_aim(Aimed::Attack, TargetRule::Enemies, Reach::One, Level::Commands, ctx),
                },
                MAGIC if ctx.model.can_cast() => {
                    self.state = State::Magic;
                    Reply::Handled
                }
                MAGIC if ctx.model.silenced => Reply::Refused(Refusal::Silenced),
                MAGIC => Reply::Refused(Refusal::NoMagic),
                ITEM if ctx.model.has_items() => {
                    self.state = State::Items;
                    Reply::Handled
                }
                ITEM => Reply::Refused(Refusal::NoItems),
                DEFEND if ctx.model.defend => Reply::Send(Choice::Defend),
                DEFEND => Reply::Refused(Refusal::NoDefend),
                ESCAPE if ctx.model.escape => Reply::Send(Choice::Escape),
                _ => Reply::Refused(Refusal::NoEscape),
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
                    ctx.model.spells.get(index).map(|s| (Aimed::Magic(s.id), s.target, s.reach, spell_refusal(s, ctx.mp)))
                } else {
                    ctx.model.items.get(index).map(|i| (Aimed::Item(i.id), i.target, i.reach, item_refusal(i)))
                };
                return match picked {
                    None => Reply::Refused(if magic { Refusal::NoMagic } else { Refusal::NoItems }),
                    Some((_, _, _, Some(why))) => Reply::Refused(why),
                    Some((what, rule, reach, None)) => self.start_aim(what, rule, reach, from, ctx),
                };
            }
        }
        Reply::Handled
    }

    /// Open the target selection for `what` on the first valid target (or the one chosen last), or on ALL when
    /// the action hits a whole side and nothing else.
    fn start_aim(&mut self, what: Aimed, rule: TargetRule, reach: Reach, from: Level, ctx: &Ctx<'_>) -> Reply {
        let candidates = ctx.candidates(rule);
        let Some(first) = candidates.first().copied() else { return Reply::Refused(Refusal::NoTarget) };
        let at = match reach {
            Reach::All => At::All,
            Reach::One | Reach::OneOrAll => At::One(self.remembered[rule.slot()].filter(|t| candidates.contains(t)).unwrap_or(first)),
        };
        self.state = State::Aiming(Aiming { what, rule, reach, at, from });
        Reply::Handled
    }

    /// The places the pointer goes through, in the order the arrows walk them: every fighter the rule allows and
    /// then, where the action can hit the whole side, ALL (an action that hits a whole side only has ALL).
    fn places(aim: &Aiming, candidates: &[ActorId]) -> Vec<At> {
        let mut places: Vec<At> = match aim.reach {
            Reach::All => Vec::new(),
            Reach::One | Reach::OneOrAll => candidates.iter().map(|t| At::One(*t)).collect(),
        };
        if aim.reach != Reach::One {
            places.push(At::All);
        }
        places
    }

    fn on_aim(&mut self, key: MenuKey, aim: Aiming, ctx: &Ctx<'_>) -> Reply {
        let candidates = ctx.candidates(aim.rule);
        let places = Self::places(&aim, &candidates);
        let here = places.iter().position(|p| *p == aim.at).unwrap_or(0);
        let n = places.len().max(1);
        let moved = |to: usize| State::Aiming(Aiming { at: places.get(to).copied().unwrap_or(aim.at), ..aim });
        match key {
            MenuKey::Up | MenuKey::Left => self.state = moved((here + n - 1) % n),
            MenuKey::Down | MenuKey::Right => self.state = moved((here + 1) % n),
            MenuKey::Back => self.state = State::of_level(aim.from),
            MenuKey::Confirm => {
                self.state = State::Commands;
                let target = match aim.at {
                    At::One(id) => {
                        self.remembered[aim.rule.slot()] = Some(id);
                        Target::One(id)
                    }
                    // The sides are the commanding actor's: the enemies, or the party (the fallen of it for a
                    // revival).
                    At::All if aim.rule == TargetRule::Enemies => Target::AllEnemies,
                    At::All => Target::AllAllies,
                };
                return Reply::Send(match (aim.what, target) {
                    (Aimed::Attack, Target::One(target)) => Choice::Attack { target },
                    // The Attack command has no whole-side way: its selection never has ALL.
                    (Aimed::Attack, _) => return Reply::Handled,
                    (Aimed::Magic(spell), target) => Choice::Magic { spell, target },
                    (Aimed::Item(item), target) => Choice::Item { item, target },
                    (Aimed::Limit, target) => Choice::Limit { target },
                });
            }
        }
        Reply::Handled
    }

    /// What to draw.
    pub fn view(&self, ctx: &Ctx<'_>) -> MenuView {
        let state = self.resolved(ctx);
        let first = if ctx.model.ready_limit().is_some() { LIMIT_LABEL } else { COMMAND_LABELS[ATTACK] };
        let commands = [
            CommandRow { label: first, enabled: true },
            CommandRow { label: COMMAND_LABELS[MAGIC], enabled: ctx.model.can_cast() },
            CommandRow { label: COMMAND_LABELS[ITEM], enabled: ctx.model.has_items() },
            CommandRow { label: COMMAND_LABELS[DEFEND], enabled: ctx.model.defend },
            CommandRow { label: COMMAND_LABELS[ESCAPE], enabled: ctx.model.escape },
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
                    MAGIC if ctx.model.silenced => Some(Refusal::Silenced.text()),
                    MAGIC if !ctx.model.has_magic() => Some(Refusal::NoMagic.text()),
                    ITEM if !ctx.model.has_items() => Some(Refusal::NoItems.text()),
                    DEFEND if !ctx.model.defend => Some(Refusal::NoDefend.text()),
                    ESCAPE if !ctx.model.escape => Some(Refusal::NoEscape.text()),
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
                // The Magic list says how many whole-side casts the All materia still allows, when a spell of the
                // list can use one.
                let all = (magic && ctx.model.spells.iter().any(|s| s.reach == Reach::OneOrAll)).then_some(ctx.model.all_left);
                view.list = Some(ListView {
                    title: if magic { "Magic" } else { "Item" },
                    rows: rows.into_iter().skip(cursor.scroll).take(LIST_ROWS).collect(),
                    cursor: cursor.index - cursor.scroll.min(cursor.index),
                    scroll: cursor.scroll,
                    total,
                    all,
                });
            }
            State::Aiming(aim) => {
                view.level = Level::Target;
                view.aim = Some(AimView {
                    at: match aim.at {
                        At::One(target) => AimAt::One(target),
                        At::All => AimAt::All,
                    },
                    enemy: aim.rule == TargetRule::Enemies,
                    revive: aim.rule == TargetRule::DeadAllies,
                    reach: aim.reach,
                });
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
    /// The Magic list only, when a spell in it can hit a whole side with an All materia use: how many uses
    /// are left this battle (the list's title row says `ALL x1`).
    pub all: Option<u8>,
}

/// Where the pointer of an open target selection is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AimAt {
    /// On one fighter.
    One(ActorId),
    /// On the whole side: every fighter the action hits.
    All,
}

/// The pointer of an open target selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AimView {
    pub at: AimAt,
    /// The target is on the enemy side.
    pub enemy: bool,
    /// The action brings back the fallen: a whole side is the fallen of the party, not the living.
    pub revive: bool,
    /// What else the pointer can be on besides one fighter ([`Reach::OneOrAll`]: ALL, a step past the last one).
    pub reach: Reach,
}

impl AimView {
    /// The fighter the pointer is on, if it is on one.
    pub fn one(&self) -> Option<ActorId> {
        match self.at {
            AimAt::One(id) => Some(id),
            AimAt::All => None,
        }
    }

    /// Whether the pointer is on `actor` (one fighter), or on the whole side and `actor`, a fighter of the side the
    /// pointer is on (the caller says which: `enemy`), is one the action hits (the living, or the fallen for a
    /// revival: `alive` says which `actor` is).
    pub fn covers(&self, actor: ActorId, alive: bool) -> bool {
        match self.at {
            AimAt::One(id) => id == actor,
            AimAt::All => alive != self.revive,
        }
    }
}

/// Everything the panels draw of the menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuView {
    pub level: Level,
    pub commands: [CommandRow; 5],
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
            limit: None,
            escape: true,
            all_left: 0,
            silenced: false,
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
        assert_eq!(m.view(&s.ctx()).command_cursor, 4, "up from the top wraps to Escape");
        assert_eq!(press(&mut m, &s, &[Down]), Reply::Handled);
        assert_eq!(m.view(&s.ctx()).command_cursor, 0, "down from the bottom wraps to Attack");
        // Enter on Attack opens the target selection on the first enemy, a second Enter attacks it.
        assert_eq!(press(&mut m, &s, &[Confirm]), Reply::Handled);
        assert_eq!(m.level(), Level::Target);
        let aim = m.view(&s.ctx()).aim.expect("a pointer");
        assert_eq!((aim.one(), aim.enemy), (Some(ActorId(3)), true));
        assert_eq!(press(&mut m, &s, &[Confirm]), Reply::Send(Choice::Attack { target: ActorId(3) }));
        assert_eq!(m.level(), Level::Commands, "the menu is back at the top once a choice is made");
    }

    #[test]
    fn target_cycling_wraps_skips_the_dead_and_remembers_the_last_choice() {
        let mut s = Scene::new();
        s.enemies[1].alive = false; // actor 4 is dead
        let mut m = BattleMenu::default();
        press(&mut m, &s, &[Confirm]);
        let at = |m: &BattleMenu, s: &Scene| m.view(&s.ctx()).aim.expect("aim").one().expect("a fighter").0;
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
            assert_eq!(m.view(&s.ctx()).aim.expect("aim").one(), Some(ActorId(4)));
        }
        // No living enemy at all: Attack cannot even open the selection.
        let mut m = BattleMenu::default();
        s.enemies = fighters(&[false; 3], 3);
        assert_eq!(press(&mut m, &s, &[Confirm]), Reply::Refused(Refusal::NoTarget));
        assert_eq!(m.level(), Level::Commands);
    }

    // --- whole sides (protocol v10) ---

    /// Fire and Cure can hit a whole side while the character has an All use, Ultima hits a whole side only.
    fn side_scene() -> Scene {
        let mut s = Scene::new();
        s.model.spells[0].reach = Reach::OneOrAll; // Fire
        s.model.spells[2].reach = Reach::OneOrAll; // Cure
        s.model.spells.push(MenuSpell::new(4, "Ultima", 7, TargetRule::Enemies).with_reach(Reach::All));
        s.model.all_left = 1;
        s
    }

    fn aim_of(m: &BattleMenu, s: &Scene) -> AimView {
        m.view(&s.ctx()).aim.expect("a pointer")
    }

    #[test]
    fn a_spell_that_can_hit_a_whole_side_has_an_all_place_one_step_past_the_last_target() {
        let s = side_scene();
        let mut m = BattleMenu::default();
        // Magic, Fire: the pointer starts on the first enemy, as it always did.
        assert_eq!(press(&mut m, &s, &[Down, Confirm, Confirm]), Reply::Handled);
        let aim = aim_of(&m, &s);
        assert_eq!((aim.at, aim.enemy, aim.reach), (AimAt::One(ActorId(3)), true, Reach::OneOrAll));
        // Three enemies, then ALL, then round to the first again.
        for expected in [AimAt::One(ActorId(4)), AimAt::One(ActorId(5)), AimAt::All, AimAt::One(ActorId(3))] {
            press(&mut m, &s, &[Right]);
            assert_eq!(aim_of(&m, &s).at, expected);
        }
        // Left from the first goes to ALL, and one more step to the last.
        press(&mut m, &s, &[Left]);
        let all = aim_of(&m, &s);
        assert_eq!(all.at, AimAt::All);
        assert!(all.enemy && all.covers(ActorId(3), true) && all.covers(ActorId(5), true), "every living enemy is on it");
        assert!(!all.covers(ActorId(4), false), "and not a fallen one");
        press(&mut m, &s, &[Up]);
        assert_eq!(aim_of(&m, &s).at, AimAt::One(ActorId(5)));
        // Confirm on ALL sends the spell at every enemy, and leaves the target remembered alone.
        press(&mut m, &s, &[Down]);
        assert_eq!(press(&mut m, &s, &[Confirm]), Reply::Send(Choice::Magic { spell: 1, target: Target::AllEnemies }));
        assert_eq!(m.level(), Level::Commands);
        press(&mut m, &s, &[Confirm, Confirm]);
        assert_eq!(aim_of(&m, &s).at, AimAt::One(ActorId(3)), "the next aim starts on a fighter, never on ALL");
    }

    #[test]
    fn cure_on_all_allies_is_the_whole_party_and_a_single_target_spell_has_no_all_place() {
        let s = side_scene();
        let mut m = BattleMenu::default();
        // Magic, Cure (the third row): the party.
        press(&mut m, &s, &[Down, Confirm, Down, Down, Confirm]);
        assert_eq!((aim_of(&m, &s).at, aim_of(&m, &s).enemy), (AimAt::One(ActorId(0)), false));
        press(&mut m, &s, &[Left]);
        assert_eq!(aim_of(&m, &s).at, AimAt::All);
        assert_eq!(press(&mut m, &s, &[Confirm]), Reply::Send(Choice::Magic { spell: 3, target: Target::AllAllies }));
        // Ice is the kind that hits one target: the pointer only walks the three enemies. (The cursors stayed where
        // they were: Magic, and Cure in its list.)
        press(&mut m, &s, &[Confirm, Up, Confirm]);
        let mut seen = Vec::new();
        for _ in 0..4 {
            seen.push(aim_of(&m, &s).at);
            press(&mut m, &s, &[Right]);
        }
        assert_eq!(seen, [AimAt::One(ActorId(3)), AimAt::One(ActorId(4)), AimAt::One(ActorId(5)), AimAt::One(ActorId(3))]);
        assert_eq!(aim_of(&m, &s).reach, Reach::One);
    }

    #[test]
    fn what_only_hits_a_whole_side_starts_on_all_and_the_arrows_cannot_leave_it() {
        let s = side_scene();
        let mut m = BattleMenu::default();
        // Magic, Ultima (the last row).
        press(&mut m, &s, &[Down, Confirm, Down, Down, Down, Confirm]);
        let aim = aim_of(&m, &s);
        assert_eq!((aim.at, aim.enemy, aim.reach), (AimAt::All, true, Reach::All));
        for k in [Up, Down, Left, Right] {
            assert_eq!(press(&mut m, &s, &[k]), Reply::Handled);
            assert_eq!(aim_of(&m, &s).at, AimAt::All, "{k:?}");
        }
        // Back goes to the list with the cursor kept; Confirm sends the whole side.
        assert_eq!(press(&mut m, &s, &[Back]), Reply::Handled);
        assert_eq!(m.level(), Level::Magic);
        press(&mut m, &s, &[Confirm]);
        assert_eq!(press(&mut m, &s, &[Confirm]), Reply::Send(Choice::Magic { spell: 4, target: Target::AllEnemies }));
    }

    #[test]
    fn all_means_the_living_or_for_a_revival_the_fallen_and_the_pointer_follows_the_roster() {
        let mut s = side_scene();
        s.allies[1].alive = false;
        s.model.items[1].reach = Reach::OneOrAll; // a revival of the kind that can hit a whole side
        let mut m = BattleMenu::default();
        // Item, Phoenix Down (the second row): the fallen member, then ALL.
        press(&mut m, &s, &[Down, Down, Confirm, Down, Confirm]);
        assert_eq!(aim_of(&m, &s).at, AimAt::One(ActorId(1)));
        press(&mut m, &s, &[Right]);
        let all = aim_of(&m, &s);
        assert_eq!((all.at, all.enemy, all.revive), (AimAt::All, false, true));
        assert!(all.covers(ActorId(1), false) && !all.covers(ActorId(0), true), "the fallen only");
        assert_eq!(press(&mut m, &s, &[Confirm]), Reply::Send(Choice::Item { item: 11, target: Target::AllAllies }));

        // The pointer on ALL stays there while anybody is on the side, and the menu steps back a level when the
        // whole side is gone.
        let mut s = side_scene();
        let mut m = BattleMenu::default();
        press(&mut m, &s, &[Down, Confirm, Down, Down, Down, Confirm]);
        s.enemies[0].alive = false;
        assert_eq!(aim_of(&m, &s).at, AimAt::All, "two enemies are left");
        s.enemies = fighters(&[false; 3], 3);
        assert_eq!(m.view(&s.ctx()).level, Level::Magic, "nobody to hit: back to the list");
        assert_eq!(press(&mut m, &s, &[Back]), Reply::Handled);
        assert_eq!(m.level(), Level::Commands);
    }

    #[test]
    fn a_limit_that_hits_the_party_starts_on_all_and_sends_the_whole_party() {
        let mut s = Scene::new();
        s.model.limit = Some(MenuLimit::new(142, "Healing Wind", TargetRule::Allies, true).with_reach(Reach::All));
        let mut m = BattleMenu::default();
        // The first command says Limit; its pointer is ALL.
        assert_eq!(press(&mut m, &s, &[Confirm]), Reply::Handled);
        let aim = aim_of(&m, &s);
        assert_eq!((aim.at, aim.enemy, aim.reach), (AimAt::All, false, Reach::All));
        assert_eq!(press(&mut m, &s, &[Confirm]), Reply::Send(Choice::Limit { target: Target::AllAllies }));
        // A limit that strikes one enemy is as it was.
        s.model.limit = Some(MenuLimit::new(128, "Braver", TargetRule::Enemies, true));
        press(&mut m, &s, &[Confirm]);
        assert_eq!((aim_of(&m, &s).at, aim_of(&m, &s).reach), (AimAt::One(ActorId(3)), Reach::One));
    }

    #[test]
    fn the_magic_list_says_how_many_all_uses_are_left_when_a_spell_of_it_can_use_one() {
        let mut s = side_scene();
        press(&mut BattleMenu::default(), &s, &[]);
        let mut m = BattleMenu::default();
        press(&mut m, &s, &[Down, Confirm]);
        assert_eq!(m.view(&s.ctx()).list.expect("the Magic list").all, Some(1));
        s.model.all_left = 3;
        assert_eq!(m.view(&s.ctx()).list.expect("the Magic list").all, Some(3));
        // No spell that can use one (the client gives the kind that needs an All use only while there is one):
        // nothing to say. The Item list never says it.
        for spell in &mut s.model.spells {
            spell.reach = Reach::One;
        }
        assert_eq!(m.view(&s.ctx()).list.expect("the Magic list").all, None);
        press(&mut m, &s, &[Back, Down, Confirm]);
        assert_eq!(m.view(&s.ctx()).list.expect("the Item list").all, None);
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
        assert_eq!((aim.one(), aim.enemy), (Some(ActorId(0)), false), "Cure aims at the party");
        press(&mut m, &s, &[Right]);
        assert_eq!(press(&mut m, &s, &[Confirm]), Reply::Send(Choice::Magic { spell: 3, target: Target::One(ActorId(1)) }));
    }

    #[test]
    fn an_attack_spell_aims_at_enemies_and_back_returns_to_the_list_with_the_cursor_kept() {
        let s = Scene::new();
        let mut m = BattleMenu::default();
        press(&mut m, &s, &[Down, Confirm, Down, Confirm]);
        assert_eq!(m.level(), Level::Target);
        let aim = m.view(&s.ctx()).aim.expect("aim");
        assert_eq!((aim.one(), aim.enemy), (Some(ActorId(3)), true));
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
            assert_eq!((aim.one(), aim.enemy), (Some(ActorId(2)), false));
        }
        assert_eq!(press(&mut m, &s, &[Confirm]), Reply::Send(Choice::Item { item: 11, target: Target::One(ActorId(2)) }));
        // A potion aims at the living, and the empty vial cannot be picked.
        let mut m = BattleMenu::default();
        press(&mut m, &s, &[Down, Down, Confirm, Confirm]);
        let aim = m.view(&s.ctx()).aim.expect("aim");
        assert_eq!(aim.one(), Some(ActorId(0)));
        press(&mut m, &s, &[Right, Right]);
        assert_eq!(m.view(&s.ctx()).aim.expect("aim").one(), Some(ActorId(0)), "only two are alive, so two steps come full circle");
        press(&mut m, &s, &[Back, Down, Down]);
        assert_eq!(m.view(&s.ctx()).hint, Some("None left"));
        assert_eq!(press(&mut m, &s, &[Confirm]), Reply::Refused(Refusal::OutOfItem));
    }

    // --- silence ---

    #[test]
    fn silence_greys_out_magic_and_nothing_else_and_says_why() {
        let mut s = Scene::new();
        s.model.silenced = true;
        let mut m = BattleMenu::default();
        let v = m.view(&s.ctx());
        let flags: Vec<(&str, bool)> = v.commands.iter().map(|c| (c.label, c.enabled)).collect();
        assert_eq!(flags, [("Attack", true), ("Magic", false), ("Item", true), ("Defend", true), ("Escape", true)]);
        // On Magic the line under the status band says why, and Enter is refused with the same words.
        assert_eq!(press(&mut m, &s, &[Down]), Reply::Handled);
        assert_eq!(m.view(&s.ctx()).hint, Some("Silenced: cannot cast"));
        assert_eq!(press(&mut m, &s, &[Confirm]), Reply::Refused(Refusal::Silenced));
        assert_eq!(m.level(), Level::Commands);
        // The others still work: Item opens, Attack aims.
        assert_eq!(press(&mut m, &s, &[Down, Confirm]), Reply::Handled);
        assert_eq!(m.level(), Level::Items);
        press(&mut m, &s, &[Back, Up, Up]);
        assert_eq!(press(&mut m, &s, &[Confirm]), Reply::Handled);
        assert_eq!(m.level(), Level::Target);
        // A voice that is there and an empty list are the old refusal (it says nothing of silence).
        let mut quiet = Scene::new();
        quiet.model.spells.clear();
        let mut m = BattleMenu::default();
        assert_eq!(press(&mut m, &quiet, &[Down, Confirm]), Reply::Refused(Refusal::NoMagic));
    }

    #[test]
    fn a_voice_lost_with_the_magic_list_or_a_spells_target_open_closes_them() {
        let mut s = Scene::new();
        let mut m = BattleMenu::default();
        // The Magic list is open on Ice...
        press(&mut m, &s, &[Down, Confirm, Down]);
        assert_eq!(m.level(), Level::Magic);
        // ...and the voice goes: the next look at the menu is the command list, on Magic, greyed.
        s.model.silenced = true;
        let v = m.view(&s.ctx());
        assert_eq!((v.level, v.command_cursor, v.hint), (Level::Commands, 1, Some("Silenced: cannot cast")));
        assert_eq!(press(&mut m, &s, &[Confirm]), Reply::Refused(Refusal::Silenced));
        // A spell's target selection closes too; an item's does not.
        s.model.silenced = false;
        let mut m = BattleMenu::default();
        press(&mut m, &s, &[Down, Confirm, Confirm]);
        assert_eq!(m.level(), Level::Target, "aiming Fire");
        s.model.silenced = true;
        assert_eq!(m.view(&s.ctx()).level, Level::Commands);
        s.model.silenced = false;
        let mut m = BattleMenu::default();
        press(&mut m, &s, &[Down, Down, Confirm, Confirm]);
        assert_eq!(m.level(), Level::Target, "aiming a Potion");
        s.model.silenced = true;
        assert_eq!(m.view(&s.ctx()).level, Level::Target);
    }

    #[test]
    fn what_the_model_lacks_is_greyed_out_and_cannot_be_opened() {
        let mut s = Scene::new();
        s.model = BattleMenuModel::default();
        let mut m = BattleMenu::default();
        let v = m.view(&s.ctx());
        let flags: Vec<(&str, bool)> = v.commands.iter().map(|c| (c.label, c.enabled)).collect();
        assert_eq!(flags, [("Attack", true), ("Magic", false), ("Item", false), ("Defend", false), ("Escape", false)]);
        assert_eq!(press(&mut m, &s, &[Down, Confirm]), Reply::Refused(Refusal::NoMagic));
        assert_eq!(m.view(&s.ctx()).hint, Some("No magic to cast"));
        assert_eq!(press(&mut m, &s, &[Down, Confirm]), Reply::Refused(Refusal::NoItems));
        assert_eq!(press(&mut m, &s, &[Down, Confirm]), Reply::Refused(Refusal::NoDefend));
        assert_eq!(press(&mut m, &s, &[Down, Confirm]), Reply::Refused(Refusal::NoEscape));
        assert_eq!(m.view(&s.ctx()).hint, Some("Cannot escape from this battle"));
        assert_eq!(m.level(), Level::Commands);
        // Attack still works.
        assert_eq!(press(&mut m, &s, &[Down, Confirm]), Reply::Handled);
        assert_eq!(m.level(), Level::Target);
    }

    #[test]
    fn defend_sends_at_once_when_the_engine_has_it() {
        let s = Scene::new();
        let mut m = BattleMenu::default();
        assert_eq!(press(&mut m, &s, &[Down, Down, Down, Confirm]), Reply::Send(Choice::Defend));
        assert_eq!(m.level(), Level::Commands);
    }

    #[test]
    fn escape_is_the_last_row_sends_at_once_and_is_greyed_out_when_the_battle_cannot_be_run_from() {
        let mut s = Scene::new();
        let mut m = BattleMenu::default();
        assert_eq!(press(&mut m, &s, &[Up, Confirm]), Reply::Send(Choice::Escape));
        assert_eq!(m.level(), Level::Commands, "no target, no list: the menu is back at the top");
        assert_eq!(Choice::Escape.to_command(), Command::Escape);
        let v = m.view(&s.ctx());
        assert_eq!((v.commands[4].label, v.commands[4].enabled, v.hint), ("Escape", true, None));
        // A boss: the row is there but dim, says why, and sends nothing.
        s.model.escape = false;
        let v = m.view(&s.ctx());
        assert_eq!((v.commands[4].label, v.commands[4].enabled, v.hint), ("Escape", false, Some("Cannot escape from this battle")));
        assert_eq!(press(&mut m, &s, &[Confirm]), Reply::Refused(Refusal::NoEscape));
        assert_eq!(m.level(), Level::Commands);
    }

    fn with_limit(target: TargetRule, ready: bool) -> Scene {
        let mut s = Scene::new();
        s.model.limit = Some(MenuLimit::new(128, "Braver", target, ready));
        s
    }

    #[test]
    fn a_full_limit_gauge_puts_limit_where_attack_was_and_aims_at_the_limits_own_side() {
        // Not full: the first row is Attack and does what Attack does.
        let s = with_limit(TargetRule::Enemies, false);
        let mut m = BattleMenu::default();
        assert_eq!(m.view(&s.ctx()).commands[0].label, "Attack");
        assert_eq!(press(&mut m, &s, &[Confirm, Confirm]), Reply::Send(Choice::Attack { target: ActorId(3) }));

        // Full: the same row says Limit, is enabled, and Confirm aims among the enemies, the second Confirm
        // gives the limit on the first living one.
        let s = with_limit(TargetRule::Enemies, true);
        let mut m = BattleMenu::default();
        let v = m.view(&s.ctx());
        assert_eq!((v.commands[0].label, v.commands[0].enabled, v.command_cursor), ("Limit", true, 0));
        assert_eq!(press(&mut m, &s, &[Confirm]), Reply::Handled);
        let aim = m.view(&s.ctx()).aim.expect("a pointer");
        assert_eq!((aim.one(), aim.enemy), (Some(ActorId(3)), true));
        assert_eq!(press(&mut m, &s, &[Right, Confirm]), Reply::Send(Choice::Limit { target: Target::One(ActorId(4)) }));
        assert_eq!(Choice::Limit { target: Target::One(ActorId(4)) }.to_command(), Command::Limit { target: Target::One(ActorId(4)) });
        assert_eq!(m.level(), Level::Commands);
        // Back from the pointer returns to the commands, where the row still says Limit.
        press(&mut m, &s, &[Confirm]);
        assert_eq!(press(&mut m, &s, &[Back]), Reply::Handled);
        assert_eq!(m.view(&s.ctx()).commands[0].label, "Limit");

        // A healing limit (Healing Wind) aims at the party, not at the enemies.
        let s = with_limit(TargetRule::Allies, true);
        let mut m = BattleMenu::default();
        assert_eq!(press(&mut m, &s, &[Confirm]), Reply::Handled);
        let aim = m.view(&s.ctx()).aim.expect("a pointer");
        assert_eq!((aim.one(), aim.enemy), (Some(ActorId(0)), false));
        assert_eq!(press(&mut m, &s, &[Down, Confirm]), Reply::Send(Choice::Limit { target: Target::One(ActorId(1)) }));

        // The other rows do not care: Magic still opens its list, and the gauge emptying puts Attack back.
        let mut s = with_limit(TargetRule::Enemies, true);
        let mut m = BattleMenu::default();
        assert_eq!(press(&mut m, &s, &[Down, Confirm]), Reply::Handled);
        assert_eq!(m.level(), Level::Magic);
        press(&mut m, &s, &[Back]);
        s.model.limit.as_mut().expect("a limit").ready = false;
        assert_eq!(m.view(&s.ctx()).commands[0].label, "Attack");
        // A character with no limit at all never shows one.
        s.model.limit = None;
        assert_eq!(m.view(&s.ctx()).commands[0].label, "Attack");
    }

    #[test]
    fn a_limit_with_nobody_to_aim_at_refuses_like_attack_and_a_dead_target_is_replaced() {
        let mut s = with_limit(TargetRule::Enemies, true);
        let mut m = BattleMenu::default();
        s.enemies = fighters(&[false; 3], 3);
        assert_eq!(press(&mut m, &s, &[Confirm]), Reply::Refused(Refusal::NoTarget));
        assert_eq!(m.level(), Level::Commands);
        // The aimed-at enemy dies under the pointer: the pointer moves on, as it does for Attack.
        s.enemies = fighters(&[true; 3], 3);
        press(&mut m, &s, &[Confirm]);
        s.enemies[0].alive = false;
        assert_eq!(press(&mut m, &s, &[Confirm]), Reply::Send(Choice::Limit { target: Target::One(ActorId(4)) }));
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
        assert_eq!(press(&mut m, &s, &[Confirm]), Reply::Send(Choice::Magic { spell: 5, target: Target::One(ActorId(3)) }));
    }

    #[test]
    fn a_target_that_dies_under_the_pointer_is_replaced_and_a_wiped_out_side_goes_back_a_level() {
        let mut s = Scene::new();
        let mut m = BattleMenu::default();
        press(&mut m, &s, &[Down, Confirm, Confirm]);
        assert_eq!(m.view(&s.ctx()).aim.expect("aim").one(), Some(ActorId(3)));
        // The aimed-at enemy dies while the pointer is on it: the view and the next key agree on the
        // replacement.
        s.enemies[0].alive = false;
        assert_eq!(m.view(&s.ctx()).aim.expect("aim").one(), Some(ActorId(4)));
        assert_eq!(press(&mut m, &s, &[Confirm]), Reply::Send(Choice::Magic { spell: 1, target: Target::One(ActorId(4)) }));
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
    fn choices_become_engine_commands_one_to_one() {
        assert_eq!(Choice::Attack { target: ActorId(4) }.to_command(), Command::Attack { target: ActorId(4) });
        assert_eq!(Choice::Magic { spell: 27, target: Target::One(ActorId(3)) }.to_command(), Command::Magic { spell: 27, target: Target::One(ActorId(3)) });
        assert_eq!(Choice::Item { item: 7, target: Target::One(ActorId(1)) }.to_command(), Command::Item { item: 7, target: Target::One(ActorId(1)) });
        assert_eq!(Choice::Defend.to_command(), Command::Defend);
    }

    /// What the menu sends is what the engine takes, spell for spell and item for item: a choice made
    /// through the keys comes out as the command that names the same ids and the same target.
    #[test]
    fn a_choice_made_with_the_keys_is_the_engine_command_of_the_same_ids_and_target() {
        let s = Scene::new();
        let mut m = BattleMenu::default();
        // Cure (spell 3 of this model) on the second ally, a Potion (the pointer starts on the ally
        // chosen last), Defend.
        let Reply::Send(cure) = press(&mut m, &s, &[Down, Confirm, Down, Down, Confirm, Right, Confirm]) else { panic!("a choice") };
        assert_eq!(cure.to_command(), Command::Magic { spell: 3, target: Target::One(ActorId(1)) });
        let Reply::Send(potion) = press(&mut m, &s, &[Down, Confirm, Confirm, Confirm]) else { panic!("a choice") };
        assert_eq!(potion.to_command(), Command::Item { item: 10, target: Target::One(ActorId(1)) });
        let Reply::Send(defend) = press(&mut m, &s, &[Down, Confirm]) else { panic!("a choice") };
        assert_eq!(defend.to_command(), Command::Defend);
    }

    /// Whatever is pressed in whatever order, the menu never panics, never leaves a level that does
    /// not exist, and only ever sends a choice whose target the rule allows.
    #[test]
    fn random_keys_over_changing_rosters_stay_sane() {
        use crate::ui::testkit::Rng;
        let mut rng = Rng::new(0x00B4_771E);
        let keys = [Up, Down, Left, Right, Confirm, Back];
        for round in 0..200 {
            let mut s = Scene::new();
            let mut m = BattleMenu::default();
            // Whom the limit aims at is a fact of the character (Healing Wind heals, Braver strikes), not
            // something that changes during a battle; whether its gauge is full, and the roster, do.
            let limit_target = if rng.chance(2) { TargetRule::Enemies } else { TargetRule::Allies };
            // How many each thing can be aimed at is a fact of it too (one, one or all, only all).
            let reaches = [Reach::One, Reach::OneOrAll, Reach::All];
            for spell in &mut s.model.spells {
                spell.reach = reaches[rng.below(3)];
            }
            for item in &mut s.model.items {
                item.reach = reaches[rng.below(3)];
            }
            let limit_reach = reaches[rng.below(3)];
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
                    // A limit that is sometimes full (or the character has none), an escape that is sometimes barred.
                    s.model.limit = (!rng.chance(3)).then(|| MenuLimit::new(128, "Limit", limit_target, rng.chance(2)).with_reach(limit_reach));
                    s.model.escape = rng.chance(2);
                }
                let before = m.state;
                let reply = m.press(keys[rng.below(keys.len())], &s.ctx());
                if let Reply::Send(choice) = reply {
                    // What the choice was aimed with is what the open selection started with (the model may have
                    // changed under it since, as it does at every message): its rule and its reach.
                    let aimed = || match before {
                        State::Aiming(aiming) => (aiming.rule, aiming.reach),
                        other => panic!("round {round} step {step}: {choice:?} was sent from {other:?}, not from a target selection"),
                    };
                    let (target, (rule, reach)) = match choice {
                        Choice::Attack { target } => (Some(Target::One(target)), aimed()),
                        Choice::Magic { target, .. } | Choice::Item { target, .. } | Choice::Limit { target } => (Some(target), aimed()),
                        Choice::Defend | Choice::Escape => (None, (TargetRule::Enemies, Reach::One)),
                    };
                    let candidates = s.ctx().candidates(rule);
                    match target {
                        Some(Target::One(t)) => {
                            assert!(candidates.contains(&t), "round {round} step {step}: {choice:?} aims at a fighter the rule does not allow");
                            assert!(reach != Reach::All, "round {round} step {step}: {choice:?} aims at one what only hits a whole side");
                        }
                        // A whole side: the side the rule means, for something that can hit one, with somebody on it.
                        Some(side @ (Target::AllEnemies | Target::AllAllies)) => {
                            assert!(reach != Reach::One, "round {round} step {step}: {choice:?} aims at a whole side what hits one target");
                            assert_eq!(side == Target::AllEnemies, rule == TargetRule::Enemies, "round {round} step {step}: {choice:?} is the wrong side");
                            assert!(!candidates.is_empty(), "round {round} step {step}: {choice:?} hits a side with nobody on it");
                        }
                        None => {}
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
