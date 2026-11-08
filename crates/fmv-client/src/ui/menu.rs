//! The pre-join menu: choose a saved character, or create one. A pure state machine: key events
//! and results go in, [`MenuAction`]s come out, nothing here touches the disk, the network or the
//! GPU. [`crate::ui::screens`] draws it, [`crate::ui::shell`] runs the actions.
//!
//! # Screens
//!
//! * **Select**: the characters saved for this server (arrows, Enter plays, `N` creates, `Del`
//!   asks to forget a saved login on THIS machine, `Esc` quits). Forgetting is confirmed first
//!   and never touches the server.
//! * **Create**: name, look, tint (a palette grid and three sliders), then Create or Back. Enter
//!   registers (wherever the focus is, except on Back); Esc goes back.
//! * **Connecting**: an attempt is running; Esc abandons it.
//! * **Loading**: the area is being prepared after the join.
//!
//! Whatever the server answers to an attempt comes back through [`Menu::connect_failed`] as a
//! [`Notice`] shown on screen; the screen that started the attempt is restored (or, when the
//! attempt created an account whose token was saved before it failed, the list with that
//! character highlighted) so the player can simply try again.

use super::entry::TextEntry;
use super::palette;
use crate::cli::tint_from_name;
use fmv_core::GameId;
use fmv_protocol::Secret;

// ---------------------------------------------------------------------------------------------
// Looks
// ---------------------------------------------------------------------------------------------

/// One look offered on the create screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppearanceOption {
    /// The stable id sent to the server (`Hello::appearance_ref`) and stored in the avatar
    /// profile. Adapters map it to a model.
    pub id: &'static str,
    pub label: &'static str,
}

/// The FF7 field models a character can wear (a parallel piece of work maps these ids to models
/// decoded from the owner's own copy; until then the create screen shows the names and a
/// coloured marker).
pub const FF7_APPEARANCES: [AppearanceOption; 4] = [
    AppearanceOption { id: "ff7/cloud", label: "Cloud Strife" },
    AppearanceOption { id: "ff7/tifa", label: "Tifa Lockhart" },
    AppearanceOption { id: "ff7/barret", label: "Barret Wallace" },
    AppearanceOption { id: "ff7/aerith", label: "Aerith Gainsborough" },
];

/// The looks offered for `game`'s content. Games without models of their own (the synthetic
/// FIXTURE) offer none: the character is the plain coloured marker.
pub fn appearance_options(game: GameId) -> &'static [AppearanceOption] {
    match game {
        GameId::Ff7 => &FF7_APPEARANCES,
        _ => &[],
    }
}

/// A short human label for any appearance id: a known one by its name, an unknown one as it is
/// (cleaned, so text from a server cannot carry control characters), none as `default marker`.
pub fn appearance_label(id: Option<&str>) -> String {
    match id {
        None => "default marker".to_string(),
        Some(id) => match FF7_APPEARANCES.iter().find(|o| o.id == id) {
            Some(o) => o.label.to_string(),
            None => {
                let cleaned = crate::net::clean(id);
                if cleaned.trim().is_empty() {
                    "default marker".to_string()
                } else {
                    cleaned
                }
            }
        },
    }
}

/// How a character looks, as last seen from this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Look {
    pub tint: [u8; 3],
    pub appearance: Option<String>,
}

/// A saved character as the select screen lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharacterEntry {
    /// The display name (cleaned).
    pub name: String,
    /// `None` until this machine has seen the character join once (the server keeps the look;
    /// the saved login does not).
    pub look: Option<Look>,
    /// The token file can be read by other users (shown as a warning).
    pub loose_permissions: bool,
}

// ---------------------------------------------------------------------------------------------
// Input and output
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavKey {
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Enter,
    Escape,
    Tab,
    Backspace,
    Delete,
}

/// A key press. `repeat` is the auto-repeat of a held key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyPress {
    pub key: NavKey,
    pub shift: bool,
    pub repeat: bool,
}

#[cfg(test)]
impl KeyPress {
    pub fn new(key: NavKey) -> Self {
        Self { key, shift: false, repeat: false }
    }

    pub fn shifted(key: NavKey) -> Self {
        Self { key, shift: true, repeat: false }
    }

    pub fn repeating(key: NavKey) -> Self {
        Self { key, shift: false, repeat: true }
    }
}

/// What the menu asks its host to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuAction {
    /// Close the client.
    Quit,
    /// Log in with the saved character at this index of [`Menu::accounts`].
    Play { index: usize },
    /// Create a character: register `name` with this tint and look.
    Register { name: String, tint: [u8; 3], appearance: Option<String> },
    /// Forget the saved login of the character at this index (locally; never on the server).
    Forget { index: usize },
    /// Abandon the running connection attempt.
    CancelConnect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeKind {
    Error,
    Warning,
    Info,
}

/// A message shown on screen (a server's refusal, a local problem).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub kind: NoticeKind,
    pub text: String,
    /// What the player can do about it.
    pub hint: Option<String>,
    /// Something the player must be able to read and copy, which may not be covered by anything
    /// else: the token of an account that was created but whose token could not be saved. A
    /// notice with a secret takes a whole screen and stays until Enter or Escape.
    pub secret: Option<Secret>,
}

impl Notice {
    pub fn error(text: impl Into<String>) -> Self {
        Self { kind: NoticeKind::Error, text: text.into(), hint: None, secret: None }
    }

    pub fn info(text: impl Into<String>) -> Self {
        Self { kind: NoticeKind::Info, text: text.into(), hint: None, secret: None }
    }

    pub fn warning(text: impl Into<String>) -> Self {
        Self { kind: NoticeKind::Warning, text: text.into(), hint: None, secret: None }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn with_secret(mut self, secret: impl Into<String>) -> Self {
        self.secret = Some(Secret::new(secret));
        self
    }

    /// Does this notice take the whole screen?
    pub fn is_alert(&self) -> bool {
        self.secret.is_some()
    }
}

// ---------------------------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------------------------

/// What the screens need to know about where the player is going.
#[derive(Debug, Clone)]
pub struct MenuConfig {
    /// The server, as shown (`host:port`).
    pub server: String,
    /// The game whose content this client holds.
    pub game: GameId,
    /// A colour asked for on the command line (`--tint`): the create form starts with it chosen
    /// instead of following the name.
    pub tint: Option<[u8; 3]>,
}

impl MenuConfig {
    pub fn new(server: impl Into<String>, game: GameId) -> Self {
        Self { server: server.into(), game, tint: None }
    }

    /// Start the create form with this colour (`--tint`).
    pub fn with_tint(mut self, tint: Option<[u8; 3]>) -> Self {
        self.tint = tint;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmChoice {
    Keep,
    Forget,
}

/// The "forget this saved login?" question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Confirm {
    /// Index into [`Menu::accounts`].
    pub index: usize,
    /// Starts on `Keep`: forgetting needs a deliberate move or the `Y` key.
    pub focus: ConfirmChoice,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectState {
    pub cursor: usize,
    pub confirm: Option<Confirm>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Name,
    Look,
    Palette,
    Red,
    Green,
    Blue,
    Create,
    Back,
}

const FIELDS_WITH_LOOKS: [Field; 8] =
    [Field::Name, Field::Look, Field::Palette, Field::Red, Field::Green, Field::Blue, Field::Create, Field::Back];
const FIELDS_WITHOUT_LOOKS: [Field; 7] =
    [Field::Name, Field::Palette, Field::Red, Field::Green, Field::Blue, Field::Create, Field::Back];

/// Step of an RGB slider per key press (`Shift` makes it 1), and of PageUp/PageDown.
pub const SLIDER_STEP: i16 = 8;
pub const SLIDER_STEP_FINE: i16 = 1;
pub const SLIDER_STEP_COARSE: i16 = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateForm {
    pub name: TextEntry,
    pub tint: [u8; 3],
    /// The player picked a colour: from now on the tint stays, instead of following the name
    /// (until then it is the name-derived tint the command line would have used).
    pub tint_chosen: bool,
    pub palette_cursor: usize,
    /// Index into [`appearance_options`] (meaningless when there are none).
    pub appearance: usize,
    pub focus: Field,
    has_looks: bool,
}

impl CreateForm {
    fn new(cfg: &MenuConfig) -> Self {
        let tint = tint_from_name("");
        let mut form = Self {
            name: TextEntry::for_names(),
            tint,
            tint_chosen: false,
            palette_cursor: palette::nearest(tint),
            appearance: 0,
            focus: Field::Name,
            has_looks: !appearance_options(cfg.game).is_empty(),
        };
        if let Some(asked) = cfg.tint {
            form.palette_cursor = palette::nearest(asked);
            form.choose_tint(asked);
        }
        form
    }

    /// The fields in focus order.
    pub fn fields(&self) -> &'static [Field] {
        if self.has_looks {
            &FIELDS_WITH_LOOKS
        } else {
            &FIELDS_WITHOUT_LOOKS
        }
    }

    /// Does the form have a look row (the game has models to choose from)?
    pub fn has_looks(&self) -> bool {
        self.has_looks
    }

    fn focus_index(&self) -> usize {
        self.fields().iter().position(|f| *f == self.focus).unwrap_or(0)
    }

    fn focus_next(&mut self) {
        let f = self.fields();
        self.focus = f[(self.focus_index() + 1) % f.len()];
    }

    fn focus_prev(&mut self) {
        let f = self.fields();
        self.focus = f[(self.focus_index() + f.len() - 1) % f.len()];
    }

    /// The tint follows the name until the player chooses one.
    fn follow_name(&mut self) {
        if !self.tint_chosen {
            self.tint = tint_from_name(&self.name.name().unwrap_or_default());
            self.palette_cursor = palette::nearest(self.tint);
        }
    }

    fn choose_tint(&mut self, tint: [u8; 3]) {
        self.tint = tint;
        self.tint_chosen = true;
        if let Some(i) = palette::index_of(tint) {
            self.palette_cursor = i;
        }
    }

    fn move_palette(&mut self, dcol: i32, drow: i32) {
        let (col, row) = palette::cell(self.palette_cursor);
        let col = (col as i32 + dcol).rem_euclid(palette::COLS as i32) as usize;
        let row = (row as i32 + drow).clamp(0, palette::ROWS as i32 - 1) as usize;
        self.palette_cursor = palette::index_at(col, row);
        self.choose_tint(palette::PALETTE[self.palette_cursor]);
    }

    fn adjust_channel(&mut self, channel: usize, delta: i16) {
        let mut tint = self.tint;
        tint[channel] = (i16::from(tint[channel]) + delta).clamp(0, 255) as u8;
        self.choose_tint(tint);
    }

    fn set_channel(&mut self, channel: usize, value: u8) {
        let mut tint = self.tint;
        tint[channel] = value;
        self.choose_tint(tint);
    }

    fn cycle_look(&mut self, game: GameId, forward: bool) {
        let n = appearance_options(game).len();
        if n > 0 {
            self.appearance = if forward { (self.appearance + 1) % n } else { (self.appearance + n - 1) % n };
        }
    }
}

/// What a connection attempt is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Login { name: String },
    Register { name: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Connecting {
    pub target: Target,
    /// [`Menu::clock`] when the attempt started (the spinner and the elapsed seconds).
    pub since: f32,
    /// The screen to return to if the attempt fails or is abandoned.
    pub previous: Box<Screen>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Screen {
    Select(SelectState),
    Create(CreateForm),
    Connecting(Connecting),
    /// Joined: preparing the area (what is being loaded).
    Loading(String),
}

/// What a game controller's buttons are about to act on, which decides what a few of them mean (see
/// [`crate::ui::input::pad_menu_input`]): the same keys do different things on different screens,
/// and some of them must never do anything dangerous by accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PadScreen {
    /// The list of saved characters.
    List,
    /// The "forget this saved login?" question over the list.
    Question,
    /// The create form; `can_go_back`: there is a list to go back to.
    Form { can_go_back: bool },
    /// A connection attempt is running.
    Connecting,
    /// The area is being prepared after the join.
    Loading,
    /// A notice that takes the whole screen (a token that could not be saved) and must be read.
    Alert,
}

/// The index of the first row shown when `rows` rows of a list of `count` entries fit on screen
/// and `cursor` is the selected one: the cursor stays in the middle until the ends of the list.
pub fn list_window(cursor: usize, count: usize, rows: usize) -> usize {
    if rows == 0 || count <= rows {
        return 0;
    }
    cursor.saturating_sub(rows / 2).min(count - rows)
}

#[derive(Debug, Clone)]
pub struct Menu {
    cfg: MenuConfig,
    accounts: Vec<CharacterEntry>,
    screen: Screen,
    notice: Option<Notice>,
    clock: f32,
}

impl Menu {
    /// A menu over the saved characters of the server. It opens on the list, or, when nothing is
    /// saved yet, directly on the create form.
    pub fn new(cfg: MenuConfig, accounts: Vec<CharacterEntry>) -> Self {
        let screen = if accounts.is_empty() {
            Screen::Create(CreateForm::new(&cfg))
        } else {
            Screen::Select(SelectState { cursor: 0, confirm: None })
        };
        Self { cfg, accounts, screen, notice: None, clock: 0.0 }
    }

    // ----- queries ------------------------------------------------------------------------

    pub fn config(&self) -> &MenuConfig {
        &self.cfg
    }

    pub fn accounts(&self) -> &[CharacterEntry] {
        &self.accounts
    }

    pub fn screen(&self) -> &Screen {
        &self.screen
    }

    pub fn notice(&self) -> Option<&Notice> {
        self.notice.as_ref()
    }

    /// Seconds since the menu opened (for the blinking cursor and the spinner).
    pub fn clock(&self) -> f32 {
        self.clock
    }

    /// The looks the create screen offers.
    pub fn appearances(&self) -> &'static [AppearanceOption] {
        appearance_options(self.cfg.game)
    }

    /// The name of the highlighted saved character (select screen only).
    pub fn selected_name(&self) -> Option<&str> {
        match &self.screen {
            Screen::Select(s) => self.accounts.get(s.cursor).map(|a| a.name.as_str()),
            _ => None,
        }
    }

    /// Is a connection attempt running?
    pub fn is_connecting(&self) -> bool {
        matches!(self.screen, Screen::Connecting(_))
    }

    /// Is a text field in focus (the name field of the create form)? That is where a keyboard
    /// belongs: the window asks Steam for its on-screen keyboard when this becomes true. A notice
    /// that takes the whole screen covers the form, so then nothing is in focus. (There is no
    /// password field: if one is ever added it is masked by the field itself, and the keyboard
    /// request is the same.)
    pub fn text_input_focused(&self) -> bool {
        !self.notice.as_ref().is_some_and(Notice::is_alert) && matches!(&self.screen, Screen::Create(f) if f.focus == Field::Name)
    }

    /// What a controller's buttons are about to act on.
    pub fn pad_screen(&self) -> PadScreen {
        if self.notice.as_ref().is_some_and(Notice::is_alert) {
            return PadScreen::Alert;
        }
        match &self.screen {
            Screen::Select(s) if s.confirm.is_some() => PadScreen::Question,
            Screen::Select(_) => PadScreen::List,
            Screen::Create(_) => PadScreen::Form { can_go_back: !self.accounts.is_empty() },
            Screen::Connecting(_) => PadScreen::Connecting,
            Screen::Loading(_) => PadScreen::Loading,
        }
    }

    // ----- feeding ------------------------------------------------------------------------

    /// Time passes (`dt` seconds).
    pub fn tick(&mut self, dt: f32) {
        if dt.is_finite() && dt > 0.0 {
            self.clock += dt.min(1.0);
        }
    }

    /// Show `text` as the current message.
    pub fn set_notice(&mut self, notice: Notice) {
        self.notice = Some(notice);
    }

    /// The joined area is being prepared.
    pub fn loading(&mut self, what: impl Into<String>) {
        self.screen = Screen::Loading(what.into());
        self.notice = None;
    }

    /// The saved characters changed on disk (a token was saved or forgotten, possibly by a
    /// connection attempt that ended after it was abandoned). The highlighted character stays
    /// highlighted if it still exists; an empty list opens the create form.
    pub fn refresh_accounts(&mut self, accounts: Vec<CharacterEntry>) {
        if accounts == self.accounts {
            return;
        }
        let selected = self.selected_name().map(str::to_string);
        self.accounts = accounts;
        match &mut self.screen {
            Screen::Select(s) => {
                s.confirm = None;
                s.cursor = selected
                    .and_then(|name| self.accounts.iter().position(|a| a.name == name))
                    .unwrap_or_else(|| s.cursor.min(self.accounts.len().saturating_sub(1)));
                if self.accounts.is_empty() {
                    self.screen = Screen::Create(CreateForm::new(&self.cfg));
                }
            }
            Screen::Connecting(c) => {
                // The screen to come back to may be a list whose indices just changed.
                if let Screen::Select(s) = c.previous.as_mut() {
                    s.confirm = None;
                    s.cursor = s.cursor.min(self.accounts.len().saturating_sub(1));
                }
            }
            Screen::Create(_) | Screen::Loading(_) => {}
        }
    }

    /// A screen that is about to be shown again, made valid for the list as it is now: a list
    /// with nothing in it is the create form, and a cursor never points past the end.
    fn restored(&self, screen: Screen) -> Screen {
        match screen {
            Screen::Select(_) if self.accounts.is_empty() => Screen::Create(CreateForm::new(&self.cfg)),
            Screen::Select(mut s) => {
                s.cursor = s.cursor.min(self.accounts.len() - 1);
                s.confirm = None;
                Screen::Select(s)
            }
            other => other,
        }
    }

    /// A connection attempt failed (or could not even start): show `notice` and give the player
    /// back the screen it started from. If the attempt registered a character whose token is
    /// already saved ([`refresh_accounts`](Self::refresh_accounts) must have been called first),
    /// the player lands on that character in the list instead: retrying the registration would
    /// be refused as a taken name.
    pub fn connect_failed(&mut self, notice: Notice) {
        if let Screen::Connecting(c) = &mut self.screen {
            let previous = std::mem::replace(c.previous.as_mut(), Screen::Loading(String::new()));
            let registered = match &c.target {
                Target::Register { name } => self.accounts.iter().position(|a| a.name.to_lowercase() == name.to_lowercase()),
                Target::Login { .. } => None,
            };
            let screen = match (registered, previous) {
                (Some(cursor), _) => Screen::Select(SelectState { cursor, confirm: None }),
                // Back on the form with the cursor in the name: that is what usually needs changing.
                (None, Screen::Create(mut form)) => {
                    form.focus = Field::Name;
                    Screen::Create(form)
                }
                (None, previous) => previous,
            };
            self.screen = self.restored(screen);
        }
        self.notice = Some(notice);
    }

    /// A key was pressed. Returns what the host has to do, if anything.
    pub fn key(&mut self, press: KeyPress) -> Option<MenuAction> {
        // Keys that do something once never auto-repeat: a held Enter must not start a second
        // attempt, a held Delete must not walk through a confirmation.
        if press.repeat && matches!(press.key, NavKey::Enter | NavKey::Escape | NavKey::Delete | NavKey::Tab) {
            return None;
        }
        // A secret on screen (a token that could not be saved) is answered first, and by itself:
        // no key may do anything else until it was read and dismissed.
        if self.notice.as_ref().is_some_and(Notice::is_alert) {
            if matches!(press.key, NavKey::Enter | NavKey::Escape) {
                self.notice = None;
            }
            return None;
        }
        match self.screen {
            Screen::Select(_) => self.select_key(press),
            Screen::Create(_) => self.create_key(press),
            Screen::Connecting(_) => match press.key {
                NavKey::Escape => self.abandon(),
                _ => None,
            },
            Screen::Loading(_) => (press.key == NavKey::Escape).then_some(MenuAction::Quit),
        }
    }

    /// Text was typed (what the keyboard produced for a press, with modifiers applied).
    pub fn text(&mut self, text: &str) -> Option<MenuAction> {
        if self.notice.as_ref().is_some_and(Notice::is_alert) {
            return None;
        }
        let mut action = None;
        for c in text.chars() {
            let a = match self.screen {
                Screen::Select(_) => self.select_char(c),
                Screen::Create(_) => self.create_char(c),
                Screen::Connecting(_) | Screen::Loading(_) => None,
            };
            action = action.or(a);
        }
        action
    }

    // ----- select -------------------------------------------------------------------------

    fn select_key(&mut self, press: KeyPress) -> Option<MenuAction> {
        let n = self.accounts.len();
        let Screen::Select(s) = &mut self.screen else { return None };
        if let Some(confirm) = &mut s.confirm {
            return match press.key {
                NavKey::Left | NavKey::Right | NavKey::Up | NavKey::Down | NavKey::Tab => {
                    confirm.focus = match confirm.focus {
                        ConfirmChoice::Keep => ConfirmChoice::Forget,
                        ConfirmChoice::Forget => ConfirmChoice::Keep,
                    };
                    None
                }
                NavKey::Enter => {
                    let c = *confirm;
                    s.confirm = None;
                    (c.focus == ConfirmChoice::Forget).then_some(MenuAction::Forget { index: c.index })
                }
                NavKey::Escape => {
                    s.confirm = None;
                    None
                }
                _ => None,
            };
        }
        match press.key {
            NavKey::Up if n > 0 => s.cursor = (s.cursor + n - 1) % n,
            NavKey::Down if n > 0 => s.cursor = (s.cursor + 1) % n,
            NavKey::Home => s.cursor = 0,
            NavKey::End => s.cursor = n.saturating_sub(1),
            NavKey::PageUp => s.cursor = s.cursor.saturating_sub(5),
            NavKey::PageDown => s.cursor = (s.cursor + 5).min(n.saturating_sub(1)),
            NavKey::Enter if n > 0 => {
                let index = s.cursor.min(n - 1);
                return Some(self.begin(Target::Login { name: self.accounts[index].name.clone() }, MenuAction::Play { index }));
            }
            NavKey::Delete if n > 0 => {
                s.confirm = Some(Confirm { index: s.cursor.min(n - 1), focus: ConfirmChoice::Keep });
            }
            NavKey::Escape => return Some(MenuAction::Quit),
            _ => {}
        }
        None
    }

    fn select_char(&mut self, c: char) -> Option<MenuAction> {
        let Screen::Select(s) = &mut self.screen else { return None };
        if let Some(confirm) = s.confirm {
            return match c.to_ascii_lowercase() {
                'y' => {
                    s.confirm = None;
                    Some(MenuAction::Forget { index: confirm.index })
                }
                'n' => {
                    s.confirm = None;
                    None
                }
                _ => None,
            };
        }
        if c.eq_ignore_ascii_case(&'n') {
            self.screen = Screen::Create(CreateForm::new(&self.cfg));
            self.notice = None;
        }
        None
    }

    // ----- create -------------------------------------------------------------------------

    fn create_key(&mut self, press: KeyPress) -> Option<MenuAction> {
        let game = self.cfg.game;
        let can_go_back = !self.accounts.is_empty();
        let Screen::Create(f) = &mut self.screen else { return None };
        let key = press.key;
        match key {
            NavKey::Escape => return self.leave_create(can_go_back),
            NavKey::Tab => {
                if press.shift {
                    f.focus_prev()
                } else {
                    f.focus_next()
                }
            }
            NavKey::Enter => {
                return if f.focus == Field::Back { self.leave_create(can_go_back) } else { self.submit() };
            }
            NavKey::Up => match f.focus {
                Field::Palette if palette::cell(f.palette_cursor).1 > 0 => f.move_palette(0, -1),
                _ => f.focus_prev(),
            },
            NavKey::Down => match f.focus {
                Field::Palette if palette::cell(f.palette_cursor).1 + 1 < palette::ROWS => f.move_palette(0, 1),
                _ => f.focus_next(),
            },
            NavKey::Left | NavKey::Right => {
                let forward = key == NavKey::Right;
                let step = if press.shift { SLIDER_STEP_FINE } else { SLIDER_STEP };
                let signed = if forward { step } else { -step };
                match f.focus {
                    Field::Name => {
                        if forward {
                            f.name.right();
                        } else {
                            f.name.left();
                        }
                    }
                    Field::Look => f.cycle_look(game, forward),
                    Field::Palette => f.move_palette(if forward { 1 } else { -1 }, 0),
                    Field::Red => f.adjust_channel(0, signed),
                    Field::Green => f.adjust_channel(1, signed),
                    Field::Blue => f.adjust_channel(2, signed),
                    Field::Create | Field::Back => f.focus = if forward { Field::Back } else { Field::Create },
                }
            }
            NavKey::Home => match f.focus {
                Field::Name => f.name.home(),
                Field::Red => f.set_channel(0, 0),
                Field::Green => f.set_channel(1, 0),
                Field::Blue => f.set_channel(2, 0),
                _ => {}
            },
            NavKey::End => match f.focus {
                Field::Name => f.name.end(),
                Field::Red => f.set_channel(0, 255),
                Field::Green => f.set_channel(1, 255),
                Field::Blue => f.set_channel(2, 255),
                _ => {}
            },
            NavKey::PageUp | NavKey::PageDown => {
                let signed = if key == NavKey::PageUp { SLIDER_STEP_COARSE } else { -SLIDER_STEP_COARSE };
                match f.focus {
                    Field::Red => f.adjust_channel(0, signed),
                    Field::Green => f.adjust_channel(1, signed),
                    Field::Blue => f.adjust_channel(2, signed),
                    _ => {}
                }
            }
            NavKey::Backspace => {
                if f.focus == Field::Name && f.name.backspace() {
                    f.follow_name();
                }
            }
            NavKey::Delete => {
                if f.focus == Field::Name && f.name.delete() {
                    f.follow_name();
                }
            }
        }
        None
    }

    fn create_char(&mut self, c: char) -> Option<MenuAction> {
        let game = self.cfg.game;
        let can_go_back = !self.accounts.is_empty();
        let Screen::Create(f) = &mut self.screen else { return None };
        match (f.focus, c) {
            (Field::Name, c) => {
                if f.name.insert(c) {
                    f.follow_name();
                }
                None
            }
            (Field::Look, ' ') => {
                f.cycle_look(game, true);
                None
            }
            (Field::Create, ' ') => self.submit(),
            (Field::Back, ' ') => self.leave_create(can_go_back),
            _ => None,
        }
    }

    /// Escape or Back: to the list if there is one, else out of the client.
    fn leave_create(&mut self, can_go_back: bool) -> Option<MenuAction> {
        if !can_go_back {
            return Some(MenuAction::Quit);
        }
        self.screen = Screen::Select(SelectState { cursor: 0, confirm: None });
        self.notice = None;
        None
    }

    /// Enter on the create form: check the name and start registering.
    fn submit(&mut self) -> Option<MenuAction> {
        let game = self.cfg.game;
        let Screen::Create(f) = &mut self.screen else { return None };
        let Some(name) = f.name.name() else {
            f.focus = Field::Name;
            self.notice = Some(Notice::error("Type a name for your character first.").with_hint("Up to 24 characters."));
            return None;
        };
        if self.accounts.iter().any(|a| a.name.to_lowercase() == name.to_lowercase()) {
            f.focus = Field::Name;
            self.notice = Some(
                Notice::error(format!("You already have a saved character called {name} on this server."))
                    .with_hint("Press Esc and pick it from the list, or choose another name."),
            );
            return None;
        }
        let appearance = appearance_options(game).get(f.appearance).map(|o| o.id.to_string());
        let action = MenuAction::Register { name: name.clone(), tint: f.tint, appearance };
        Some(self.begin(Target::Register { name }, action))
    }

    // ----- connecting ---------------------------------------------------------------------

    /// Move to the connecting screen, remembering where to come back to.
    fn begin(&mut self, target: Target, action: MenuAction) -> MenuAction {
        let previous = std::mem::replace(&mut self.screen, Screen::Loading(String::new()));
        self.screen = Screen::Connecting(Connecting { target, since: self.clock, previous: Box::new(previous) });
        self.notice = None;
        action
    }

    /// Escape while connecting: back to where the attempt started.
    fn abandon(&mut self) -> Option<MenuAction> {
        if let Screen::Connecting(c) = &mut self.screen {
            let previous = std::mem::replace(c.previous.as_mut(), Screen::Loading(String::new()));
            self.screen = self.restored(previous);
            self.notice = Some(Notice::info("Cancelled."));
        }
        Some(MenuAction::CancelConnect)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SERVER: &str = "127.0.0.1:47770";

    fn cfg(game: GameId) -> MenuConfig {
        MenuConfig::new(SERVER, game)
    }

    fn entry(name: &str) -> CharacterEntry {
        CharacterEntry { name: name.to_string(), look: None, loose_permissions: false }
    }

    fn menu_with(names: &[&str]) -> Menu {
        Menu::new(cfg(GameId::Ff7), names.iter().map(|n| entry(n)).collect())
    }

    fn press(m: &mut Menu, key: NavKey) -> Option<MenuAction> {
        m.key(KeyPress::new(key))
    }

    fn form(m: &Menu) -> &CreateForm {
        match m.screen() {
            Screen::Create(f) => f,
            other => panic!("not the create form: {other:?}"),
        }
    }

    fn cursor(m: &Menu) -> usize {
        match m.screen() {
            Screen::Select(s) => s.cursor,
            other => panic!("not the list: {other:?}"),
        }
    }

    fn type_name(m: &mut Menu, name: &str) {
        assert_eq!(m.text(name), None);
    }

    // ---- opening -----------------------------------------------------------------------------

    #[test]
    fn it_opens_on_the_list_when_something_is_saved_and_on_the_create_form_otherwise() {
        let m = menu_with(&["Cloud", "Tifa"]);
        assert!(matches!(m.screen(), Screen::Select(s) if s.cursor == 0 && s.confirm.is_none()));
        assert_eq!(m.selected_name(), Some("Cloud"));
        assert!(m.notice().is_none());

        let m = menu_with(&[]);
        assert!(matches!(m.screen(), Screen::Create(_)));
        assert_eq!(form(&m).focus, Field::Name);
        assert!(form(&m).name.is_empty());
        assert_eq!(m.selected_name(), None);
    }

    // ---- select ------------------------------------------------------------------------------

    #[test]
    fn arrows_move_the_highlight_and_wrap_around() {
        let mut m = menu_with(&["Cloud", "Tifa", "Barret"]);
        press(&mut m, NavKey::Down);
        assert_eq!(cursor(&m), 1);
        press(&mut m, NavKey::Down);
        press(&mut m, NavKey::Down);
        assert_eq!(cursor(&m), 0, "past the last wraps to the first");
        press(&mut m, NavKey::Up);
        assert_eq!(cursor(&m), 2, "above the first wraps to the last");
        assert_eq!(m.key(KeyPress::repeating(NavKey::Up)), None);
        assert_eq!(cursor(&m), 1, "a held arrow keeps moving");
        press(&mut m, NavKey::Home);
        assert_eq!(cursor(&m), 0);
        press(&mut m, NavKey::End);
        assert_eq!(cursor(&m), 2);
        press(&mut m, NavKey::PageUp);
        assert_eq!(cursor(&m), 0);
        press(&mut m, NavKey::PageDown);
        assert_eq!(cursor(&m), 2);
    }

    #[test]
    fn enter_plays_the_highlighted_character_and_the_menu_waits_for_the_answer() {
        let mut m = menu_with(&["Cloud", "Tifa"]);
        press(&mut m, NavKey::Down);
        assert_eq!(press(&mut m, NavKey::Enter), Some(MenuAction::Play { index: 1 }));
        match m.screen() {
            Screen::Connecting(c) => {
                assert_eq!(c.target, Target::Login { name: "Tifa".into() });
                assert!(matches!(c.previous.as_ref(), Screen::Select(s) if s.cursor == 1));
            }
            other => panic!("{other:?}"),
        }
        assert!(m.is_connecting());
        // While connecting nothing but Escape does anything.
        for key in [NavKey::Enter, NavKey::Down, NavKey::Delete, NavKey::Tab] {
            assert_eq!(press(&mut m, key), None);
        }
        assert_eq!(m.text("n"), None);
        assert!(m.is_connecting());
    }

    #[test]
    fn a_held_enter_starts_only_one_attempt() {
        let mut m = menu_with(&["Cloud"]);
        assert_eq!(press(&mut m, NavKey::Enter), Some(MenuAction::Play { index: 0 }));
        assert_eq!(m.key(KeyPress::repeating(NavKey::Enter)), None);
        // Even before the attempt began, a repeat is not a press.
        let mut m = menu_with(&["Cloud"]);
        assert_eq!(m.key(KeyPress::repeating(NavKey::Enter)), None);
        assert!(!m.is_connecting());
    }

    #[test]
    fn escape_on_the_list_quits_and_escape_while_connecting_comes_back() {
        let mut m = menu_with(&["Cloud"]);
        assert_eq!(press(&mut m, NavKey::Escape), Some(MenuAction::Quit));
        press(&mut m, NavKey::Enter);
        assert_eq!(press(&mut m, NavKey::Escape), Some(MenuAction::CancelConnect));
        assert!(matches!(m.screen(), Screen::Select(s) if s.cursor == 0));
        assert_eq!(m.notice().map(|n| n.kind), Some(NoticeKind::Info));
        // Quitting is a press, not a repeat: a held Escape does not close the next screen too.
        assert_eq!(m.key(KeyPress::repeating(NavKey::Escape)), None);
    }

    #[test]
    fn a_failed_login_returns_to_the_list_with_the_message_and_the_same_character_highlighted() {
        let mut m = menu_with(&["Cloud", "Tifa"]);
        press(&mut m, NavKey::Down);
        press(&mut m, NavKey::Enter);
        m.connect_failed(Notice::error("cannot connect to 127.0.0.1:47770").with_hint("Press Enter to try again."));
        assert_eq!(cursor(&m), 1);
        let n = m.notice().unwrap();
        assert_eq!(n.kind, NoticeKind::Error);
        assert!(n.text.contains("cannot connect") && n.hint.as_deref() == Some("Press Enter to try again."));
        // And it can be tried again straight away.
        assert_eq!(press(&mut m, NavKey::Enter), Some(MenuAction::Play { index: 1 }));
        assert!(m.notice().is_none(), "a new attempt clears the old message");
    }

    #[test]
    fn n_opens_the_create_form_and_escape_comes_back_to_the_list() {
        let mut m = menu_with(&["Cloud"]);
        assert_eq!(m.text("n"), None);
        assert!(matches!(m.screen(), Screen::Create(_)));
        assert_eq!(press(&mut m, NavKey::Escape), None);
        assert!(matches!(m.screen(), Screen::Select(_)));
        assert_eq!(m.text("N"), None, "the capital works too");
        assert!(matches!(m.screen(), Screen::Create(_)));
        // With nothing saved there is no list to go back to: Escape leaves the client.
        let mut m = menu_with(&[]);
        assert_eq!(press(&mut m, NavKey::Escape), Some(MenuAction::Quit));
    }

    #[test]
    fn other_letters_on_the_list_do_nothing() {
        let mut m = menu_with(&["Cloud"]);
        assert_eq!(m.text("abcxyz y"), None);
        assert!(matches!(m.screen(), Screen::Select(s) if s.confirm.is_none()));
    }

    // ---- forgetting a saved login --------------------------------------------------------------

    #[test]
    fn delete_asks_first_and_the_default_answer_keeps_the_login() {
        let mut m = menu_with(&["Cloud", "Tifa"]);
        press(&mut m, NavKey::Down);
        assert_eq!(press(&mut m, NavKey::Delete), None);
        let Screen::Select(s) = m.screen() else { panic!() };
        assert_eq!(s.confirm, Some(Confirm { index: 1, focus: ConfirmChoice::Keep }));
        // Enter on the default answer keeps it: no action, the question closes.
        assert_eq!(press(&mut m, NavKey::Enter), None);
        assert!(matches!(m.screen(), Screen::Select(s) if s.confirm.is_none()));
        assert_eq!(m.accounts().len(), 2);
    }

    #[test]
    fn forgetting_needs_a_deliberate_choice() {
        let mut m = menu_with(&["Cloud", "Tifa"]);
        // Move to the other answer, then Enter.
        press(&mut m, NavKey::Delete);
        press(&mut m, NavKey::Right);
        assert!(matches!(m.screen(), Screen::Select(s) if s.confirm.map(|c| c.focus) == Some(ConfirmChoice::Forget)));
        assert_eq!(press(&mut m, NavKey::Enter), Some(MenuAction::Forget { index: 0 }));
        assert!(matches!(m.screen(), Screen::Select(s) if s.confirm.is_none()));
        // The Y key says yes at once; N and Escape say no.
        press(&mut m, NavKey::Delete);
        assert_eq!(m.text("y"), Some(MenuAction::Forget { index: 0 }));
        press(&mut m, NavKey::Delete);
        assert_eq!(m.text("n"), None);
        assert!(matches!(m.screen(), Screen::Select(s) if s.confirm.is_none()));
        press(&mut m, NavKey::Delete);
        assert_eq!(press(&mut m, NavKey::Escape), None, "Escape answers no instead of quitting");
        assert!(matches!(m.screen(), Screen::Select(s) if s.confirm.is_none()));
        // Any other letter is not an answer.
        press(&mut m, NavKey::Delete);
        assert_eq!(m.text("qxz"), None);
        assert!(matches!(m.screen(), Screen::Select(s) if s.confirm.is_some()));
    }

    #[test]
    fn a_held_delete_does_not_confirm_anything_and_other_keys_are_ignored_while_asking() {
        let mut m = menu_with(&["Cloud"]);
        press(&mut m, NavKey::Delete);
        assert_eq!(m.key(KeyPress::repeating(NavKey::Delete)), None);
        let Screen::Select(s) = m.screen() else { panic!() };
        assert_eq!(s.confirm.map(|c| c.focus), Some(ConfirmChoice::Keep));
        // Down does not move the list underneath the question.
        let mut m = menu_with(&["Cloud", "Tifa"]);
        press(&mut m, NavKey::Delete);
        press(&mut m, NavKey::PageDown);
        assert_eq!(cursor(&m), 0);
        // N does not open the create form while asking (it answers "no").
        m.text("n");
        assert!(matches!(m.screen(), Screen::Select(_)));
    }

    #[test]
    fn deleting_with_nothing_saved_is_harmless() {
        let mut m = menu_with(&["Cloud"]);
        m.refresh_accounts(vec![]);
        assert!(matches!(m.screen(), Screen::Create(_)), "an empty list becomes the create form");
        assert_eq!(press(&mut m, NavKey::Delete), None);
    }

    // ---- keeping the list in step with the disk -------------------------------------------------

    #[test]
    fn refreshing_keeps_the_highlight_on_the_same_character() {
        let mut m = menu_with(&["Aerith", "Cloud", "Tifa"]);
        press(&mut m, NavKey::Down);
        press(&mut m, NavKey::Down);
        assert_eq!(m.selected_name(), Some("Tifa"));
        // Someone saved a new character that sorts before: indices shift, the highlight does not.
        m.refresh_accounts(vec![entry("Aerith"), entry("Barret"), entry("Cloud"), entry("Tifa")]);
        assert_eq!(m.selected_name(), Some("Tifa"));
        // The highlighted one disappears (forgotten): the cursor stays in range.
        m.refresh_accounts(vec![entry("Aerith"), entry("Barret")]);
        assert_eq!(cursor(&m), 1);
        assert_eq!(m.selected_name(), Some("Barret"));
        // An identical list changes nothing, not even a confirmation in progress.
        press(&mut m, NavKey::Delete);
        m.refresh_accounts(vec![entry("Aerith"), entry("Barret")]);
        assert!(matches!(m.screen(), Screen::Select(s) if s.confirm.is_some()));
        // A changed list closes it: its index might mean someone else now.
        m.refresh_accounts(vec![entry("Barret")]);
        assert!(matches!(m.screen(), Screen::Select(s) if s.confirm.is_none()));
    }

    // ---- create -----------------------------------------------------------------------------------

    #[test]
    fn typing_goes_into_the_name_and_editing_keys_work() {
        let mut m = menu_with(&[]);
        type_name(&mut m, "Cloud");
        assert_eq!(form(&m).name.text(), "Cloud");
        press(&mut m, NavKey::Backspace);
        press(&mut m, NavKey::Left);
        press(&mut m, NavKey::Left);
        type_name(&mut m, "X");
        assert_eq!(form(&m).name.text(), "ClXou", "typed at the cursor, two to the left of the end of `Clou`");
        press(&mut m, NavKey::Home);
        press(&mut m, NavKey::Delete);
        assert_eq!(form(&m).name.text(), "lXou");
        press(&mut m, NavKey::End);
        type_name(&mut m, "d!");
        assert_eq!(form(&m).name.text(), "lXoud!");
        // Controls in typed text are dropped, and at most 24 characters fit.
        type_name(&mut m, &format!("\u{1b}{}", "z".repeat(40)));
        assert_eq!(form(&m).name.len(), 24);
        assert!(!form(&m).name.text().contains('\u{1b}'));
    }

    #[test]
    fn tab_walks_the_fields_in_order_and_wraps_and_shift_tab_goes_back() {
        let mut m = menu_with(&[]);
        let order = [Field::Look, Field::Palette, Field::Red, Field::Green, Field::Blue, Field::Create, Field::Back, Field::Name];
        for want in order {
            press(&mut m, NavKey::Tab);
            assert_eq!(form(&m).focus, want);
        }
        m.key(KeyPress::shifted(NavKey::Tab));
        assert_eq!(form(&m).focus, Field::Back);
        m.key(KeyPress::shifted(NavKey::Tab));
        assert_eq!(form(&m).focus, Field::Create);
        // Down and Up move the focus too, outside the palette.
        press(&mut m, NavKey::Up);
        assert_eq!(form(&m).focus, Field::Blue);
        press(&mut m, NavKey::Down);
        assert_eq!(form(&m).focus, Field::Create);
        // The fixture has no models to choose from, so the form has no look row.
        let mut f = Menu::new(cfg(GameId::Fixture), vec![]);
        assert!(!form(&f).has_looks());
        press(&mut f, NavKey::Tab);
        assert_eq!(form(&f).focus, Field::Palette);
        assert!(!form(&f).fields().contains(&Field::Look));
    }

    #[test]
    fn the_look_cycles_with_the_arrows_and_wraps() {
        let mut m = menu_with(&[]);
        assert_eq!(m.appearances().len(), 4);
        press(&mut m, NavKey::Tab);
        assert_eq!(form(&m).focus, Field::Look);
        press(&mut m, NavKey::Right);
        assert_eq!(form(&m).appearance, 1);
        for _ in 0..3 {
            press(&mut m, NavKey::Right);
        }
        assert_eq!(form(&m).appearance, 0, "past the last wraps to the first");
        press(&mut m, NavKey::Left);
        assert_eq!(form(&m).appearance, 3);
        m.text(" ");
        assert_eq!(form(&m).appearance, 0, "space picks the next one");
        // Typing letters while the look has the focus does not touch the name.
        m.text("abc");
        assert!(form(&m).name.is_empty());
    }

    #[test]
    fn the_tint_follows_the_name_until_the_player_chooses_a_colour() {
        let mut m = menu_with(&[]);
        assert_eq!(form(&m).tint, tint_from_name(""));
        type_name(&mut m, "Cloud");
        assert_eq!(form(&m).tint, tint_from_name("Cloud"), "the command line's default for that name");
        assert!(!form(&m).tint_chosen);
        press(&mut m, NavKey::Backspace);
        assert_eq!(form(&m).tint, tint_from_name("Clou"));
        // Choosing a colour freezes it, whatever the name does afterwards.
        press(&mut m, NavKey::Tab);
        press(&mut m, NavKey::Tab);
        assert_eq!(form(&m).focus, Field::Palette);
        press(&mut m, NavKey::Right);
        let chosen = form(&m).tint;
        assert!(form(&m).tint_chosen);
        assert_eq!(chosen, palette::PALETTE[form(&m).palette_cursor]);
        m.key(KeyPress::shifted(NavKey::Tab));
        m.key(KeyPress::shifted(NavKey::Tab));
        assert_eq!(form(&m).focus, Field::Name);
        type_name(&mut m, "dy");
        assert_eq!(form(&m).tint, chosen);
    }

    #[test]
    fn the_name_derived_tint_is_the_one_for_the_cleaned_name() {
        // `  Tifa ` registers as `Tifa`, and so the tint is the one the CLI derives for `Tifa`.
        let mut m = menu_with(&[]);
        type_name(&mut m, "  Tifa ");
        assert_eq!(form(&m).tint, tint_from_name("Tifa"));
    }

    #[test]
    fn palette_arrows_move_in_the_grid_and_leave_it_at_its_top_and_bottom() {
        let mut m = menu_with(&[]);
        for _ in 0..2 {
            press(&mut m, NavKey::Tab);
        }
        assert_eq!(form(&m).focus, Field::Palette);
        // Left wraps inside the first row, so eight presses come back to where they started.
        let start = palette::cell(form(&m).palette_cursor);
        for _ in 0..palette::COLS {
            press(&mut m, NavKey::Left);
        }
        assert_eq!(palette::cell(form(&m).palette_cursor), start);
        // Go to the top row (Up inside the grid moves up a row), then to its first column.
        if start.1 == 1 {
            press(&mut m, NavKey::Up);
        }
        while palette::cell(form(&m).palette_cursor).0 != 0 {
            press(&mut m, NavKey::Left);
        }
        assert_eq!(form(&m).focus, Field::Palette);
        assert_eq!(palette::cell(form(&m).palette_cursor), (0, 0));
        // Right and Left wrap inside the row.
        press(&mut m, NavKey::Left);
        assert_eq!(palette::cell(form(&m).palette_cursor), (palette::COLS - 1, 0));
        press(&mut m, NavKey::Right);
        assert_eq!(palette::cell(form(&m).palette_cursor), (0, 0));
        assert_eq!(form(&m).tint, palette::PALETTE[0], "the tint is the highlighted colour");
        // Down goes to the second row, a second Down leaves the grid for the next field.
        press(&mut m, NavKey::Down);
        assert_eq!(palette::cell(form(&m).palette_cursor), (0, 1));
        assert_eq!(form(&m).focus, Field::Palette);
        press(&mut m, NavKey::Down);
        assert_eq!(form(&m).focus, Field::Red);
        // Up from the second row goes to the first, from the first to the previous field.
        press(&mut m, NavKey::Up);
        assert_eq!(form(&m).focus, Field::Palette);
        assert_eq!(palette::cell(form(&m).palette_cursor), (0, 1), "coming back from below lands where it was");
        press(&mut m, NavKey::Up);
        assert_eq!(palette::cell(form(&m).palette_cursor), (0, 0));
        assert_eq!(form(&m).focus, Field::Palette);
        press(&mut m, NavKey::Up);
        assert_eq!(form(&m).focus, Field::Look);
    }

    #[test]
    fn the_sliders_step_clamp_and_jump_and_keep_the_palette_cursor_honest() {
        let mut m = menu_with(&[]);
        for _ in 0..3 {
            press(&mut m, NavKey::Tab);
        }
        assert_eq!(form(&m).focus, Field::Red);
        press(&mut m, NavKey::Home);
        assert_eq!(form(&m).tint[0], 0);
        press(&mut m, NavKey::Right);
        assert_eq!(form(&m).tint[0], 8);
        m.key(KeyPress::shifted(NavKey::Right));
        assert_eq!(form(&m).tint[0], 9, "shift is the fine step");
        press(&mut m, NavKey::PageUp);
        assert_eq!(form(&m).tint[0], 41);
        press(&mut m, NavKey::PageDown);
        press(&mut m, NavKey::PageDown);
        assert_eq!(form(&m).tint[0], 0, "clamped at 0");
        press(&mut m, NavKey::Left);
        assert_eq!(form(&m).tint[0], 0);
        press(&mut m, NavKey::End);
        assert_eq!(form(&m).tint[0], 255);
        press(&mut m, NavKey::Right);
        assert_eq!(form(&m).tint[0], 255, "clamped at 255");
        // Green and Blue are their own channels.
        press(&mut m, NavKey::Tab);
        assert_eq!(form(&m).focus, Field::Green);
        press(&mut m, NavKey::Home);
        press(&mut m, NavKey::Tab);
        assert_eq!(form(&m).focus, Field::Blue);
        press(&mut m, NavKey::End);
        let tint = form(&m).tint;
        assert_eq!((tint[0], tint[1], tint[2]), (255, 0, 255));
        assert!(form(&m).tint_chosen);
        // Dialling in a palette colour's exact value highlights it on the grid.
        let want = palette::PALETTE[3];
        for (channel, field) in [Field::Red, Field::Green, Field::Blue].into_iter().enumerate() {
            while form(&m).focus != field {
                press(&mut m, NavKey::Tab);
            }
            press(&mut m, NavKey::Home);
            for _ in 0..want[channel] / 8 {
                press(&mut m, NavKey::Right);
            }
            for _ in 0..want[channel] % 8 {
                m.key(KeyPress::shifted(NavKey::Right));
            }
        }
        assert_eq!(form(&m).tint, want);
        assert_eq!(form(&m).palette_cursor, 3);
    }

    #[test]
    fn enter_registers_the_cleaned_name_with_the_chosen_tint_and_look() {
        let mut m = menu_with(&[]);
        type_name(&mut m, "  Cloud  ");
        press(&mut m, NavKey::Tab); // look
        press(&mut m, NavKey::Right); // Tifa
        press(&mut m, NavKey::Tab); // palette
        press(&mut m, NavKey::Right);
        let tint = form(&m).tint;
        // Enter works from any field but Back: here with the focus on the palette.
        let action = press(&mut m, NavKey::Enter);
        assert_eq!(
            action,
            Some(MenuAction::Register { name: "Cloud".into(), tint, appearance: Some("ff7/tifa".into()) })
        );
        match m.screen() {
            Screen::Connecting(c) => {
                assert_eq!(c.target, Target::Register { name: "Cloud".into() });
                assert!(matches!(c.previous.as_ref(), Screen::Create(f) if f.name.text() == "  Cloud  "));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn every_look_registers_under_its_stable_id() {
        for (i, id) in ["ff7/cloud", "ff7/tifa", "ff7/barret", "ff7/aerith"].into_iter().enumerate() {
            let mut m = menu_with(&[]);
            type_name(&mut m, "P");
            press(&mut m, NavKey::Tab);
            for _ in 0..i {
                press(&mut m, NavKey::Right);
            }
            match press(&mut m, NavKey::Enter) {
                Some(MenuAction::Register { appearance, .. }) => assert_eq!(appearance.as_deref(), Some(id)),
                other => panic!("{other:?}"),
            }
        }
        // The default look (the first) is sent as `ff7/cloud`, never as "no look": what is shown is what is registered.
        let mut m = menu_with(&[]);
        type_name(&mut m, "P");
        match press(&mut m, NavKey::Enter) {
            Some(MenuAction::Register { appearance, .. }) => assert_eq!(appearance.as_deref(), Some("ff7/cloud")),
            other => panic!("{other:?}"),
        }
        // The synthetic FIXTURE has no models: no appearance is sent.
        let mut f = Menu::new(cfg(GameId::Fixture), vec![]);
        f.text("P");
        match press(&mut f, NavKey::Enter) {
            Some(MenuAction::Register { appearance, .. }) => assert_eq!(appearance, None),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_default_tint_is_registered_when_none_was_chosen() {
        let mut m = menu_with(&[]);
        type_name(&mut m, "Barret");
        match press(&mut m, NavKey::Enter) {
            Some(MenuAction::Register { tint, .. }) => assert_eq!(tint, tint_from_name("Barret")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_empty_name_is_refused_on_the_form_with_a_message() {
        let mut m = menu_with(&[]);
        assert_eq!(press(&mut m, NavKey::Enter), None);
        assert!(matches!(m.screen(), Screen::Create(_)), "still on the form");
        let n = m.notice().expect("a message");
        assert_eq!(n.kind, NoticeKind::Error);
        assert!(n.text.to_lowercase().contains("name"), "{n:?}");
        // Spaces alone are no name either, and the focus returns to the name field.
        type_name(&mut m, "   ");
        press(&mut m, NavKey::Tab);
        assert_eq!(press(&mut m, NavKey::Enter), None);
        assert_eq!(form(&m).focus, Field::Name);
        // A real name goes through, and the old message is gone.
        type_name(&mut m, "Aerith");
        assert!(matches!(press(&mut m, NavKey::Enter), Some(MenuAction::Register { .. })));
        assert!(m.notice().is_none());
    }

    #[test]
    fn a_name_that_is_already_saved_is_refused_case_insensitively() {
        let mut m = menu_with(&["Cloud"]);
        m.text("n");
        type_name(&mut m, "cLoUd");
        assert_eq!(press(&mut m, NavKey::Enter), None);
        let n = m.notice().expect("a message");
        assert!(n.text.contains("already") && n.text.contains("cLoUd"), "{n:?}");
        assert!(n.hint.as_deref().is_some_and(|h| h.contains("Esc")), "{n:?}");
        assert_eq!(form(&m).focus, Field::Name);
        // A different name is fine.
        press(&mut m, NavKey::Backspace);
        type_name(&mut m, "x");
        assert!(matches!(press(&mut m, NavKey::Enter), Some(MenuAction::Register { name, .. }) if name == "cLoUx"));
    }

    #[test]
    fn back_and_escape_leave_the_form_and_a_space_presses_the_focused_button() {
        let mut m = menu_with(&["Cloud"]);
        m.text("n");
        for _ in 0..7 {
            press(&mut m, NavKey::Tab);
        }
        assert_eq!(form(&m).focus, Field::Back);
        assert_eq!(press(&mut m, NavKey::Enter), None, "Enter on Back goes back instead of registering");
        assert!(matches!(m.screen(), Screen::Select(_)));
        // With the focus on Create, space registers.
        m.text("n");
        type_name(&mut m, "Tifa");
        while form(&m).focus != Field::Create {
            press(&mut m, NavKey::Tab);
        }
        assert!(matches!(m.text(" "), Some(MenuAction::Register { .. })));
        // Left and Right switch between the two buttons.
        let mut m = menu_with(&["Cloud"]);
        m.text("n");
        while form(&m).focus != Field::Create {
            press(&mut m, NavKey::Tab);
        }
        press(&mut m, NavKey::Right);
        assert_eq!(form(&m).focus, Field::Back);
        press(&mut m, NavKey::Left);
        assert_eq!(form(&m).focus, Field::Create);
        press(&mut m, NavKey::Right);
        assert_eq!(m.text(" "), None, "space on Back goes back");
        assert!(matches!(m.screen(), Screen::Select(_)));
        // With nothing saved the button is a Quit.
        let mut m = menu_with(&[]);
        for _ in 0..7 {
            press(&mut m, NavKey::Tab);
        }
        assert_eq!(form(&m).focus, Field::Back);
        assert_eq!(press(&mut m, NavKey::Enter), Some(MenuAction::Quit));
    }

    #[test]
    fn a_refused_registration_keeps_the_form_for_another_try() {
        let mut m = menu_with(&[]);
        type_name(&mut m, "Cloud");
        press(&mut m, NavKey::Tab);
        press(&mut m, NavKey::Right);
        let first = press(&mut m, NavKey::Enter).expect("register");
        m.connect_failed(Notice::error("that display name is already taken").with_hint("Pick another name."));
        let f = form(&m);
        assert_eq!(f.name.text(), "Cloud", "the name is still there to edit");
        assert_eq!(f.appearance, 1, "and so is the look");
        assert_eq!(f.focus, Field::Name, "the cursor is in the name field");
        assert!(m.notice().unwrap().text.contains("already taken"));
        // Editing the name keeps the message on screen until the next attempt.
        press(&mut m, NavKey::Backspace);
        type_name(&mut m, "x");
        assert!(m.notice().is_some());
        let second = press(&mut m, NavKey::Enter).expect("register again");
        assert!(m.notice().is_none());
        match (first, second) {
            (MenuAction::Register { name: a, appearance: la, .. }, MenuAction::Register { name: b, appearance: lb, .. }) => {
                assert_eq!((a.as_str(), b.as_str()), ("Cloud", "Cloux"));
                assert_eq!((la, lb), (Some("ff7/tifa".into()), Some("ff7/tifa".into())));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_registration_that_saved_its_token_and_then_failed_lands_on_that_character() {
        let mut m = menu_with(&["Aerith"]);
        m.text("n");
        type_name(&mut m, "Cloud");
        assert!(matches!(press(&mut m, NavKey::Enter), Some(MenuAction::Register { .. })));
        // The server created the account and the client saved its token, then the content check
        // failed. The host lists the saved characters again and reports the failure.
        m.refresh_accounts(vec![entry("Aerith"), entry("Cloud")]);
        m.connect_failed(Notice::error("cannot join: this machine's FF7 copy does not match the server's"));
        assert_eq!(m.selected_name(), Some("Cloud"), "retrying the registration would be refused as a taken name");
        assert!(m.notice().is_some_and(|n| n.text.contains("cannot join")));
        assert_eq!(press(&mut m, NavKey::Enter), Some(MenuAction::Play { index: 1 }));
        // A failed registration whose token was NOT saved goes back to the form instead.
        let mut m = menu_with(&["Aerith"]);
        m.text("n");
        type_name(&mut m, "Cloud");
        press(&mut m, NavKey::Enter);
        m.refresh_accounts(vec![entry("Aerith")]);
        m.connect_failed(Notice::error("registration is closed on this server"));
        assert!(matches!(m.screen(), Screen::Create(_)));
    }

    #[test]
    fn abandoning_a_registration_returns_the_form_untouched() {
        let mut m = menu_with(&[]);
        type_name(&mut m, "Cloud");
        press(&mut m, NavKey::Tab);
        press(&mut m, NavKey::Right);
        press(&mut m, NavKey::Enter);
        assert!(m.is_connecting());
        assert_eq!(press(&mut m, NavKey::Escape), Some(MenuAction::CancelConnect));
        let f = form(&m);
        assert_eq!((f.name.text().as_str(), f.appearance), ("Cloud", 1));
        // The token the abandoned attempt may still save shows up on the next refresh.
        m.refresh_accounts(vec![entry("Cloud")]);
        assert!(matches!(m.screen(), Screen::Create(_)), "a form in progress is not taken over");
        assert_eq!(m.accounts().len(), 1);
    }

    #[test]
    fn the_notice_stays_while_editing_and_goes_when_the_screen_changes() {
        let mut m = menu_with(&["Cloud"]);
        m.set_notice(Notice::warning("something"));
        press(&mut m, NavKey::Down);
        assert!(m.notice().is_some(), "navigating the list keeps it");
        m.text("n");
        assert!(m.notice().is_none(), "opening another screen clears it");
        m.set_notice(Notice::error("x"));
        press(&mut m, NavKey::Escape);
        assert!(m.notice().is_none());
    }

    #[test]
    fn loading_ignores_keys_but_escape_quits() {
        let mut m = menu_with(&["Cloud"]);
        m.loading("FF7 md1stin");
        assert!(matches!(m.screen(), Screen::Loading(w) if w == "FF7 md1stin"));
        assert_eq!(press(&mut m, NavKey::Enter), None);
        assert_eq!(m.text("n"), None);
        assert!(matches!(m.screen(), Screen::Loading(_)));
        assert_eq!(press(&mut m, NavKey::Escape), Some(MenuAction::Quit));
    }

    #[test]
    fn a_failure_with_no_attempt_running_only_shows_the_message() {
        let mut m = menu_with(&["Cloud"]);
        m.connect_failed(Notice::error("late news"));
        assert!(matches!(m.screen(), Screen::Select(_)));
        assert_eq!(m.notice().unwrap().text, "late news");
    }

    #[test]
    fn the_clock_only_moves_forward_by_sane_amounts() {
        let mut m = menu_with(&["Cloud"]);
        m.tick(0.25);
        m.tick(-1.0);
        m.tick(f32::NAN);
        m.tick(f32::INFINITY);
        m.tick(0.25);
        assert_eq!(m.clock(), 0.5);
        m.tick(100.0);
        assert_eq!(m.clock(), 1.5, "one huge step counts as at most a second");
    }

    #[test]
    fn the_list_window_keeps_the_highlight_visible_and_centred() {
        // Everything fits: no scrolling.
        assert_eq!(list_window(2, 3, 5), 0);
        assert_eq!(list_window(0, 5, 5), 0);
        assert_eq!(list_window(4, 5, 5), 0);
        assert_eq!(list_window(0, 5, 0), 0, "no room at all");
        // 20 entries, 5 rows: the cursor stays in the middle row until the ends.
        assert_eq!(list_window(0, 20, 5), 0);
        assert_eq!(list_window(2, 20, 5), 0);
        assert_eq!(list_window(10, 20, 5), 8);
        assert_eq!(list_window(19, 20, 5), 15);
        for cursor in 0..20 {
            let top = list_window(cursor, 20, 5);
            assert!(top <= cursor && cursor < top + 5, "cursor {cursor} is in view (top {top})");
            assert!(top + 5 <= 20);
        }
    }

    #[test]
    fn looks_have_labels_and_unknown_ones_are_shown_cleaned() {
        assert_eq!(appearance_label(Some("ff7/cloud")), "Cloud Strife");
        assert_eq!(appearance_label(Some("ff7/aerith")), "Aerith Gainsborough");
        assert_eq!(appearance_label(None), "default marker");
        assert_eq!(appearance_label(Some("custom/rude\u{1b}[31m")), "custom/rude[31m");
        assert_eq!(appearance_label(Some("\u{1b}")), "default marker");
        assert_eq!(appearance_options(GameId::Ff7).len(), 4);
        assert!(appearance_options(GameId::Fixture).is_empty());
        assert!(appearance_options(GameId::Ff8).is_empty());
        // The ids are the stable ones other work relies on.
        let ids: Vec<&str> = FF7_APPEARANCES.iter().map(|o| o.id).collect();
        assert_eq!(ids, ["ff7/cloud", "ff7/tifa", "ff7/barret", "ff7/aerith"]);
    }

    #[test]
    fn a_secret_on_screen_blocks_everything_until_it_is_dismissed() {
        let mut m = menu_with(&["Cloud", "Tifa"]);
        let alert = || Notice::error("the login could not be saved").with_secret("fmvt_abc");
        assert!(alert().is_alert() && !Notice::error("x").is_alert());
        m.set_notice(alert());
        // Navigating, typing and starting something do nothing while it is up.
        assert_eq!(press(&mut m, NavKey::Down), None);
        assert_eq!(cursor(&m), 0);
        assert_eq!(m.text("n"), None);
        assert!(matches!(m.screen(), Screen::Select(_)));
        assert!(m.notice().is_some());
        // A held Enter does not skip it.
        assert_eq!(m.key(KeyPress::repeating(NavKey::Enter)), None);
        assert!(m.notice().is_some());
        // Enter dismisses it without also starting a login.
        assert_eq!(press(&mut m, NavKey::Enter), None);
        assert!(m.notice().is_none() && !m.is_connecting());
        // So does Escape, without quitting.
        m.set_notice(alert());
        assert_eq!(press(&mut m, NavKey::Escape), None);
        assert!(m.notice().is_none());
        // Afterwards everything works again.
        assert_eq!(press(&mut m, NavKey::Enter), Some(MenuAction::Play { index: 0 }));
    }

    #[test]
    fn random_events_never_break_the_state_machine() {
        for seed in 1..=60 {
            crate::ui::testkit::monkey(seed, 500, |_, _| {});
        }
    }

    #[test]
    fn a_list_that_emptied_while_connecting_comes_back_as_the_create_form() {
        let mut m = menu_with(&["Cloud"]);
        press(&mut m, NavKey::Enter);
        // The only saved login was forgotten (another window) while the attempt was running.
        m.refresh_accounts(vec![]);
        m.connect_failed(Notice::error("gone"));
        assert!(matches!(m.screen(), Screen::Create(_)), "{:?}", m.screen());
        // Same when the attempt is abandoned.
        let mut m = menu_with(&["Cloud", "Tifa"]);
        press(&mut m, NavKey::Down);
        press(&mut m, NavKey::Enter);
        m.refresh_accounts(vec![entry("Cloud")]);
        assert_eq!(press(&mut m, NavKey::Escape), Some(MenuAction::CancelConnect));
        assert!(matches!(m.screen(), Screen::Select(s) if s.cursor == 0), "{:?}", m.screen());
        m.refresh_accounts(vec![]);
        assert!(matches!(m.screen(), Screen::Create(_)));
    }

    #[test]
    fn a_tint_from_the_command_line_starts_the_form_with_that_colour_chosen() {
        let with = |tint: [u8; 3]| Menu::new(MenuConfig::new(SERVER, GameId::Ff7).with_tint(Some(tint)), vec![]);
        let mut m = with([1, 2, 3]);
        assert_eq!(form(&m).tint, [1, 2, 3]);
        assert!(form(&m).tint_chosen, "it does not follow the name");
        type_name(&mut m, "Cloud");
        assert_eq!(form(&m).tint, [1, 2, 3]);
        match press(&mut m, NavKey::Enter) {
            Some(MenuAction::Register { tint, .. }) => assert_eq!(tint, [1, 2, 3]),
            other => panic!("{other:?}"),
        }
        // A colour that is on the palette grid highlights its cell; one that is not, the nearest.
        assert_eq!(form(&with(palette::PALETTE[5])).palette_cursor, 5);
        assert_eq!(form(&with([236, 81, 79])).palette_cursor, 0);
        // Without one the form follows the name as before, and so does a second form opened later.
        let mut m = Menu::new(MenuConfig::new(SERVER, GameId::Ff7).with_tint(Some([9, 9, 9])), vec![entry("Aerith")]);
        m.text("n");
        assert_eq!(form(&m).tint, [9, 9, 9], "every new form starts with the asked colour");
        let plain = Menu::new(cfg(GameId::Ff7), vec![]);
        assert!(!form(&plain).tint_chosen);
    }

    // ---- text focus and the controller's view of the screen --------------------------------------

    /// The name field is the only text field: a fresh install opens in it, moving off it leaves it,
    /// going to the form from the list enters it, and a notice over the whole screen covers it.
    #[test]
    fn the_name_field_is_the_only_text_field_and_a_covering_notice_hides_it() {
        let mut m = menu_with(&[]);
        assert!(m.text_input_focused(), "a fresh install opens in the name field");
        press(&mut m, NavKey::Down);
        assert_eq!(form(&m).focus, Field::Look);
        assert!(!m.text_input_focused());
        press(&mut m, NavKey::Up);
        assert!(m.text_input_focused());
        m.set_notice(Notice::error("the login could not be saved").with_secret("fmvt_abc"));
        assert!(!m.text_input_focused(), "the alert covers the form");
        assert_eq!(press(&mut m, NavKey::Enter), None);
        assert!(m.text_input_focused(), "dismissed: the form is back");
        // An ordinary notice does not cover anything.
        m.set_notice(Notice::info("hello"));
        assert!(m.text_input_focused());
        // The list has no text field; going to the form enters the name field.
        let mut m = menu_with(&["Cloud"]);
        assert!(!m.text_input_focused());
        m.text("n");
        assert!(m.text_input_focused());
        // Registering leaves the field (the attempt runs); a refusal puts the player back in it.
        type_name(&mut m, "Tifa");
        assert!(matches!(press(&mut m, NavKey::Enter), Some(MenuAction::Register { .. })));
        assert!(!m.text_input_focused());
        m.connect_failed(Notice::error("name taken"));
        assert!(m.text_input_focused());
    }

    #[test]
    fn the_pad_screen_says_where_a_controllers_buttons_are_heading() {
        let mut m = menu_with(&["Cloud"]);
        assert_eq!(m.pad_screen(), PadScreen::List);
        press(&mut m, NavKey::Delete);
        assert_eq!(m.pad_screen(), PadScreen::Question);
        press(&mut m, NavKey::Escape);
        assert_eq!(m.pad_screen(), PadScreen::List);
        m.text("n");
        assert_eq!(m.pad_screen(), PadScreen::Form { can_go_back: true });
        assert_eq!(menu_with(&[]).pad_screen(), PadScreen::Form { can_go_back: false }, "nothing saved: no list to go back to");
        let mut m = menu_with(&["Cloud"]);
        press(&mut m, NavKey::Enter);
        assert_eq!(m.pad_screen(), PadScreen::Connecting);
        m.loading("FF7 md1stin");
        assert_eq!(m.pad_screen(), PadScreen::Loading);
        let mut m = menu_with(&["Cloud"]);
        m.set_notice(Notice::error("the login could not be saved").with_secret("fmvt_abc"));
        assert_eq!(m.pad_screen(), PadScreen::Alert);
    }
}

