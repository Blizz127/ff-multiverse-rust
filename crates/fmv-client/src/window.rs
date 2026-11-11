//! Window mode: winit 0.30 `ApplicationHandler` + the wgpu renderer.
//!
//! The event loop is created before the session starts (see `main`), so a missing display is
//! reported cleanly before any network traffic. Every start-up failure (window, GPU) becomes an
//! error message and a non-zero exit code, never a panic.

use crate::audio::{battle_cue, Cue, GameAudio};
use crate::avatar::frame::Built;
use crate::avatar::{AvatarPlayer, Avatars, Stage, Who};
use crate::backdrop::FieldView;
use crate::camera::{CameraInput, CameraMode, FieldCamera};
use crate::battle::scene::{scene_for, BattleScene, ModelKit};
use crate::battle::{battle_vertices, draw_battle_overlay, draw_battle_text, draw_game_over, BattleCmd, MenuKey, MenuReply};
use crate::cli::exit;
use crate::gamemode::DeviceMode;
use crate::gamepad::{PadMode, Pads};
use crate::graphics::panel::{Action as GraphicsAction, Panel as GraphicsPanel};
use crate::interp::render_time;
use crate::net::PartyCommand;
use crate::npc::{npc_models, npc_players, npc_who};
use crate::render::{FrameResult, PlayerSprite, Renderer, Scene};
use crate::scene::{self, SceneRoute};
use crate::session::{Session, SessionInfo};
use crate::state::ClientState;
use crate::ui::dialogue::{field_key, Talk};
use crate::ui::draw::DrawList;
use crate::ui::hud::{self, Subject};
use fmv_core::{AnimState, GameId, Pos};
use fmv_ff7::Ff7Adapter;
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Fullscreen, Window, WindowId};

/// Walking speed as a fraction of the area's `max_speed` (the server enforces the maximum).
const WALK_SPEED_FACTOR: f32 = 0.8;
/// Frame pacing: wake up at least this often (about 60 Hz).
const FRAME: Duration = Duration::from_millis(16);
/// Longest simulated step, so a stalled frame cannot make the player jump.
const MAX_DT: f32 = 0.1;
const TITLE_REFRESH: Duration = Duration::from_secs(1);
/// How long quitting waits for `Leave` to be flushed.
const LEAVE_WAIT: Duration = Duration::from_secs(1);
/// After a first Esc in the field, a second one within this time quits.
const QUIT_CONFIRM: Duration = Duration::from_secs(3);
/// How long a notice (the quit confirmation, the battle's Esc hint) stays on screen.
const NOTICE_TIME: Duration = Duration::from_secs(3);

/// What a press of Escape does. Escape used to quit at once, which closed the window of a player
/// looking for a way out of the battle screen: in a battle it now does nothing (and says so), and
/// in the field quitting takes a second press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscapeAction {
    /// In a battle: do not quit; a notice says how to quit instead.
    IgnoreInBattle,
    /// A first press in the field: show "press Esc again to quit" and wait for a second one.
    Arm,
    /// A second press within [`QUIT_CONFIRM`] of the first: quit.
    Quit,
}

/// The action of an Escape press, `in_battle` or not, with quitting `armed` by an earlier press.
pub fn escape_action(in_battle: bool, armed: bool) -> EscapeAction {
    if in_battle {
        EscapeAction::IgnoreInBattle
    } else if armed {
        EscapeAction::Quit
    } else {
        EscapeAction::Arm
    }
}

/// Where an Escape press goes: the battle UI takes it first, else it is the window's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscapeRoute {
    /// The battle menu went back a level, or the end-of-battle panel closed (or the panel is on
    /// screen and cannot close yet): nothing else happens.
    Battle,
    /// Nobody in the battle UI wanted it: the window's own action.
    Action(EscapeAction),
}

/// Route one Escape press: back out of the battle menu (or close the panel that follows a battle)
/// if there is one open, else [`escape_action`]. In a battle this never ends in [`EscapeAction::Quit`],
/// whatever level the menu is at.
pub fn escape_route(state: &mut ClientState, now: f64, armed: bool) -> EscapeRoute {
    if state.battle.back(now) {
        EscapeRoute::Battle
    } else {
        EscapeRoute::Action(escape_action(state.battle.in_battle(), armed))
    }
}

/// Whether a first Escape press, which armed quitting until `armed_until`, still counts at `now`.
pub fn quit_armed(armed_until: Option<Instant>, now: Instant) -> bool {
    armed_until.is_some_and(|until| now < until)
}

/// Where a key event came from: the keyboard, or a game controller (`crate::gamepad`), whose events
/// are fed through the very same dispatch ([`App::key_event`]). Only the words of a notice differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputSource {
    Keyboard,
    Pad,
}

/// What an Escape press tells the player on screen, in the words of the device that pressed it
/// (`None`: nothing, the press quit). The keyboard's words are the ones the window always had.
pub fn escape_notice(action: EscapeAction, source: InputSource) -> Option<&'static str> {
    match (action, source) {
        (EscapeAction::IgnoreInBattle, InputSource::Keyboard) => Some("Esc does not quit in a battle: close the window to quit"),
        (EscapeAction::IgnoreInBattle, InputSource::Pad) => Some("Start does not quit in a battle"),
        (EscapeAction::Arm, InputSource::Keyboard) => Some("Press Esc again to quit"),
        (EscapeAction::Arm, InputSource::Pad) => Some("Press Start again to quit"),
        (EscapeAction::Quit, _) => None,
    }
}

/// Draw `text` centred in the lower part of a `vw` x `vh` window, over everything else.
fn draw_notice(text: &str, vw: u32, vh: u32, out: &mut DrawList) {
    let scale = if vw >= 1000 { 2 } else { 1 };
    let width = crate::ui::text::text_width(text, scale);
    let x = ((vw as i32 - width) / 2).max(2);
    let y = (vh as i32 * 3 / 4).max(2);
    out.draw_text_shadowed(x, y, scale, crate::ui::widgets::WARN, crate::ui::widgets::SHADOW, text);
}

pub fn create_event_loop() -> Result<EventLoop<()>, String> {
    // winit panics instead of returning an error when a system library it needs is missing
    // (for example libxkbcommon-x11 on X11). That is an environment problem, not a bug: turn
    // it into an ordinary error and keep the default panic output out of the user's way.
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let created = std::panic::catch_unwind(EventLoop::<()>::new);
    std::panic::set_hook(previous_hook);
    match created {
        Ok(Ok(event_loop)) => {
            // Which windowing system this run got (under gamescope that is X11 unless gamescope was
            // started with --expose-wayland): the device report wants to know.
            eprintln!("window: using the {} windowing backend", backend_name(&event_loop));
            Ok(event_loop)
        }
        Ok(Err(e)) => Err(friendly_os_error(&e.to_string())),
        Err(payload) => Err(format!(
            "the windowing system could not start ({}); is a required system library installed? \
             On X11 winit needs libxkbcommon-x11 and libxcb-xkb",
            panic_text(payload.as_ref())
        )),
    }
}

/// The windowing system `event_loop` runs on, for the log: on Linux winit has exactly two.
#[cfg(target_os = "linux")]
fn backend_name(event_loop: &EventLoop<()>) -> &'static str {
    use winit::platform::wayland::EventLoopExtWayland;
    if event_loop.is_wayland() {
        "Wayland"
    } else {
        "X11"
    }
}

#[cfg(not(target_os = "linux"))]
fn backend_name(_event_loop: &EventLoop<()>) -> &'static str {
    "native"
}

/// The message of a caught panic.
fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

/// winit formats OS errors as `os error at <source file>:<line>: <message>`; keep the message.
fn friendly_os_error(text: &str) -> String {
    text.strip_prefix("os error at ")
        .and_then(|rest| rest.split_once(": "))
        .map_or_else(|| text.to_string(), |(_location, message)| message.to_string())
}

/// How an event loop ended: `Err` if the windowing or GPU stack panicked.
pub type LoopResult = std::thread::Result<Result<(), winit::error::EventLoopError>>;

/// The attributes of the client's window (shared by the menu and the game, which use one window).
/// On a device that asks for it (see `crate::gamemode`) it is a borderless full-screen window on the
/// current monitor with no decorations at all: no title bar, no frame, and none of the client-side
/// decorations winit draws itself under a compositor (gamescope) that offers none.
pub fn window_attributes(title: String, device: DeviceMode) -> winit::window::WindowAttributes {
    let attributes = Window::default_attributes()
        .with_title(title)
        .with_inner_size(LogicalSize::new(1100.0, 680.0))
        .with_min_inner_size(LogicalSize::new(320.0, 240.0));
    if device.fullscreen {
        attributes.with_decorations(false).with_fullscreen(Some(Fullscreen::Borderless(None)))
    } else {
        attributes
    }
}

/// `ff7` is where the views of the areas the server moves us to are read from (see
/// [`App::with_ff7`]); `device` is what the device asks of the window (full screen).
#[allow(clippy::too_many_arguments)] // Entry point shares the menu path's presentation settings.
pub fn run(event_loop: EventLoop<()>, session: Session, view: FieldView, ff7: Option<Arc<Ff7Adapter>>, device: DeviceMode, audio: GameAudio, graphics: crate::graphics::Settings) -> u8 {
    let mut app = App::new(session, view).with_ff7(ff7).with_device(device).with_audio(audio);
    app.graphics = graphics;
    // A panic inside the windowing or GPU stack must not skip the goodbye or end as a raw abort.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| event_loop.run_app(&mut app)));
    app.finish(result)
}

/// The text of a panic or loop failure that is not a clean exit: what `run` prints.
pub fn describe_loop_failure(result: &LoopResult) -> Option<String> {
    match result {
        Err(payload) => Some(format!("the window loop crashed: {}", panic_text(payload.as_ref()))),
        Ok(Err(e)) => Some(format!("the window event loop failed: {e}")),
        Ok(Ok(())) => None,
    }
}

/// Window title. Fixture areas always say FIXTURE so they are never mistaken for the real game;
/// a real FF7 field is titled `FF Multiverse — FF7 <field>` with no FIXTURE label.
pub fn base_title(info: &SessionInfo) -> String {
    if info.fixture {
        format!("FF Multiverse - FIXTURE (synthetic, not FF7) - {}", info.name)
    } else if info.area_id.game == GameId::Ff7 {
        format!("FF Multiverse \u{2014} FF7 {}", info.area_id.key)
    } else {
        format!("FF Multiverse - {} - {}", info.area_id, info.name)
    }
}

/// The text of the window title (the only text output of the window): the base title, the player
/// count and the round-trip time, then ` | party N` while in a party and
/// ` | INVITE from <name>: press Y` while an invite addressed to us is pending. `invite_from` is
/// a cleaned display name.
pub fn live_title(base: &str, players: usize, rtt: &str, party_size: usize, invite_from: Option<&str>) -> String {
    let mut title = format!("{base} | players {players} | rtt {rtt}");
    if party_size > 0 {
        title.push_str(&format!(" | party {party_size}"));
    }
    if let Some(name) = invite_from {
        title.push_str(&format!(" | INVITE from {name}: press Y"));
    }
    title
}

/// The party keys: `P` invites the nearest other player, `Y` accepts the newest pending invite,
/// `L` leaves the party.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartyKey {
    Invite,
    Accept,
    Leave,
}

impl PartyKey {
    /// The party action a keyboard event stands for. Only a fresh PRESS counts: a release does
    /// nothing, and neither does the repeat a held key produces (holding `P` must not flood the
    /// server with invites).
    pub fn from_event(code: KeyCode, pressed: bool, repeat: bool) -> Option<Self> {
        if !pressed || repeat {
            return None;
        }
        match code {
            KeyCode::KeyP => Some(PartyKey::Invite),
            KeyCode::KeyY => Some(PartyKey::Accept),
            KeyCode::KeyL => Some(PartyKey::Leave),
            _ => None,
        }
    }
}

/// The request a party key makes in the current situation, if any: `P` invites the nearest
/// visible player who is not in our party yet (nothing if there is none), `Y` accepts the newest
/// pending invite addressed to us (nothing if there is none), `L` leaves (the server refuses it,
/// harmlessly, when we are in no party).
pub fn party_request(key: PartyKey, state: &ClientState, render_t: f64) -> Option<PartyCommand> {
    match key {
        PartyKey::Invite => state.nearest_invitable(render_t).map(PartyCommand::Invite),
        PartyKey::Accept => state.party.newest_invite_to_me().map(|invite| PartyCommand::Accept(invite.id)),
        PartyKey::Leave => Some(PartyCommand::Leave),
    }
}

/// The battle keys. `B` asks the server for a debug battle (refused unless it runs with
/// `--debug-encounters`). In a battle the arrows (and W, A, S, D) move the menu's cursor, `Space` and
/// `Enter` confirm (they open a level, pick an entry or a target), `Backspace` goes back a level
/// (so does `Esc`, see [`escape_route`]). `Tab` and `Y` swap the menu to the next ready member of the party when the
/// player commands more than one ([`KeyRoute::Swap`], not a `BattleKey`: it asks the client for no request).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BattleKey {
    Request,
    Confirm,
    Back,
    Up,
    Down,
    Left,
    Right,
}

impl BattleKey {
    /// The battle action a keyboard event stands for. Only a fresh PRESS counts for `B`, `Confirm`
    /// and `Back`, like the party keys: holding `B` must not flood the server with requests, and
    /// holding Enter must not confirm twice. The direction keys do repeat (a held arrow scrolls a long
    /// list), but only in a battle: in the field they are the movement keys and must reach
    /// [`Input::set`]. A release is never a battle key, so a movement key let go in a battle still
    /// stops the walk.
    pub fn from_event(code: KeyCode, pressed: bool, repeat: bool, in_battle: bool) -> Option<Self> {
        if !pressed {
            return None;
        }
        let fresh = !repeat;
        let direction = |key| in_battle.then_some(key);
        match code {
            KeyCode::KeyB if fresh => Some(BattleKey::Request),
            KeyCode::Space | KeyCode::Enter | KeyCode::NumpadEnter if fresh => Some(BattleKey::Confirm),
            KeyCode::Backspace if fresh => Some(BattleKey::Back),
            KeyCode::ArrowUp | KeyCode::KeyW => direction(BattleKey::Up),
            KeyCode::ArrowDown | KeyCode::KeyS => direction(BattleKey::Down),
            KeyCode::ArrowLeft | KeyCode::KeyA => direction(BattleKey::Left),
            KeyCode::ArrowRight | KeyCode::KeyD => direction(BattleKey::Right),
            _ => None,
        }
    }

    /// The menu key this stands for (`None` for `B`).
    fn menu_key(self) -> Option<MenuKey> {
        match self {
            BattleKey::Request => None,
            BattleKey::Confirm => Some(MenuKey::Confirm),
            BattleKey::Back => Some(MenuKey::Back),
            BattleKey::Up => Some(MenuKey::Up),
            BattleKey::Down => Some(MenuKey::Down),
            BattleKey::Left => Some(MenuKey::Left),
            BattleKey::Right => Some(MenuKey::Right),
        }
    }
}

/// What a battle key comes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BattleRequest {
    /// Send this to the server.
    Send(BattleCmd),
    /// The battle menu took the key (a cursor moved, a level opened or closed, a panel closed).
    Menu,
    /// Nothing happened, and why (empty when that needs no saying: a direction nobody listens to).
    Nothing(&'static str),
}

/// The request a battle key makes in the current situation: `B` outside a battle asks for one; in
/// a battle the keys drive the command menu of our character's turn, and `Confirm` on the last step
/// of a choice sends the command (see `BattleClient::menu_key`). After a battle, `Confirm` and
/// `Back` close its panel.
pub fn battle_request(key: BattleKey, state: &mut ClientState, now: f64) -> BattleRequest {
    let Some(menu_key) = key.menu_key() else {
        return if state.battle.in_battle() {
            BattleRequest::Nothing("you are already in a battle")
        } else {
            BattleRequest::Send(BattleCmd::RequestEncounter { formation: None })
        };
    };
    match state.battle.menu_key(menu_key, now) {
        MenuReply::Send(cmd) => BattleRequest::Send(cmd),
        MenuReply::Handled => BattleRequest::Menu,
        MenuReply::Refused(why) => BattleRequest::Nothing(why),
        MenuReply::Ignored => BattleRequest::Nothing(match (key, state.battle.in_battle()) {
            (BattleKey::Confirm, true) => GAUGE_NOT_FULL,
            (BattleKey::Confirm, false) => NO_BATTLE_HINT,
            _ => "",
        }),
    }
}

/// What [`battle_request`] says when Enter does nothing because the gauge is not full yet, and when there
/// is no battle at all. Neither is a refusal the game would answer with its buzzer (see `audio::battle_cue`).
pub const GAUGE_NOT_FULL: &str = "your gauge is not full yet";
pub const NO_BATTLE_HINT: &str = "no battle to fight in (press B to ask for one)";

/// The sound keys: `M` mutes and unmutes, `-` and `=` (and the keypad's `-` and `+`) lower and raise
/// the master volume by a tenth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioKey {
    Mute,
    VolumeDown,
    VolumeUp,
}

impl AudioKey {
    /// The sound action a keyboard event stands for. Only a PRESS counts, and `M` only a fresh one (a held
    /// key must not flicker the sound on and off); the volume keys repeat, so that holding one slides it.
    pub fn from_event(code: KeyCode, pressed: bool, repeat: bool) -> Option<Self> {
        if !pressed {
            return None;
        }
        match code {
            KeyCode::KeyM if !repeat => Some(AudioKey::Mute),
            KeyCode::Minus | KeyCode::NumpadSubtract => Some(AudioKey::VolumeDown),
            KeyCode::Equal | KeyCode::NumpadAdd => Some(AudioKey::VolumeUp),
            _ => None,
        }
    }
}

/// What a keyboard event is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyRoute {
    Escape,
    /// F1: show or hide the walkmesh.
    WalkMesh,
    /// F2: FF7's scrolling screen or the whole field.
    Camera,
    /// F3: optional scene edge smoothing.
    AntiAliasing,
    /// F4: native resolution or optional 2x scene rendering.
    RenderScale,
    Party(PartyKey),
    Audio(AudioKey),
    Battle(BattleKey),
    /// `Tab` or `Y` in a battle (a controller's Y is FF7's Triangle): the menu goes to the next member of the party that is
    /// ready, when the player commands more than one (see `BattleClient::swap_turn`). Outside a battle `Y` accepts an invite
    /// and `Tab` is not a key of the game.
    Swap,
    /// Everything else is for the held-key state of walking (a release always is).
    Movement,
    /// A repeat or a release of a key that acts only on a fresh press.
    Ignored,
}

/// Where a key goes. The sound keys and the battle keys come after the party keys and before walking,
/// and the direction keys are the battle's only `in_battle`, so the field keeps its arrows and WASD.
fn route_key(code: KeyCode, pressed: bool, repeat: bool, in_battle: bool) -> KeyRoute {
    // The graphics overlay handles F5 before any gameplay dispatch, including its releases.
    if code == KeyCode::F5 { return KeyRoute::Ignored; }
    if code == KeyCode::Escape {
        return if pressed && !repeat { KeyRoute::Escape } else { KeyRoute::Ignored };
    }
    if code == KeyCode::F1 {
        return if pressed && !repeat { KeyRoute::WalkMesh } else { KeyRoute::Ignored };
    }
    if code == KeyCode::F2 {
        return if pressed && !repeat { KeyRoute::Camera } else { KeyRoute::Ignored };
    }
    if code == KeyCode::F3 {
        return if pressed && !repeat { KeyRoute::AntiAliasing } else { KeyRoute::Ignored };
    }
    if code == KeyCode::F4 {
        return if pressed && !repeat { KeyRoute::RenderScale } else { KeyRoute::Ignored };
    }
    // In a battle a fresh press of Tab, or of Y (a controller's Y is FF7's Triangle), swaps the menu to the next ready member of
    // the party the player commands: FF7's own "switch". Outside a battle Y accepts an invite as ever and Tab is no key of the
    // game; a release or a repeat is not a swap and goes where it always went.
    if in_battle && pressed && !repeat && matches!(code, KeyCode::Tab | KeyCode::KeyY) {
        return KeyRoute::Swap;
    }
    if let Some(key) = PartyKey::from_event(code, pressed, repeat) {
        return KeyRoute::Party(key);
    }
    if let Some(key) = AudioKey::from_event(code, pressed, repeat) {
        return KeyRoute::Audio(key);
    }
    if let Some(key) = BattleKey::from_event(code, pressed, repeat, in_battle) {
        return KeyRoute::Battle(key);
    }
    KeyRoute::Movement
}

/// During a durable story save, only local presentation controls and an already active battle
/// remain available. Field confirmation, previews, party changes and new encounters stay blocked.
fn story_pending_route(code: KeyCode, pressed: bool, repeat: bool, battle_menu: bool) -> KeyRoute {
    let route = route_key(code, pressed, repeat, battle_menu);
    match route {
        KeyRoute::Escape | KeyRoute::WalkMesh | KeyRoute::Camera | KeyRoute::AntiAliasing
        | KeyRoute::RenderScale | KeyRoute::Audio(_) => route,
        KeyRoute::Battle(key) if battle_menu && key != BattleKey::Request => route,
        KeyRoute::Swap if battle_menu => route,
        _ => KeyRoute::Ignored,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GameOverRoute { Inactive, Held, Disconnect }

/// An acknowledged ordinary defeat has no field controls. Scene continuations keep their own
/// owner/watcher retry controls; this path only offers a clean local disconnect.
fn game_over_route(state: &ClientState, code: KeyCode, pressed: bool, repeat: bool) -> GameOverRoute {
    if !state.ordinary_game_over() { return GameOverRoute::Inactive; }
    if pressed && !repeat && matches!(code, KeyCode::Enter | KeyCode::NumpadEnter | KeyCode::Space | KeyCode::Escape | KeyCode::Backspace) {
        GameOverRoute::Disconnect
    } else {
        GameOverRoute::Held
    }
}

/// Observe every hold, including a begin and release received in one pump. A release never
/// resurrects directions that were held before the server froze the field.
fn observe_story_pending(epoch: &mut u64, observed: u64, input: &mut Input) -> bool {
    if *epoch == observed { return false; }
    *epoch = observed;
    *input = Input::default();
    true
}

/// Held movement keys.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Input {
    up: bool,
    down: bool,
    left: bool,
    right: bool,
    /// The two Shift keys (left, right): either one means walk instead of run.
    shift: [bool; 2],
}

impl Input {
    /// Record a key change; returns false for keys that are not movement keys (Shift counts: it
    /// changes how fast the movement keys move).
    pub fn set(&mut self, code: KeyCode, pressed: bool) -> bool {
        match code {
            KeyCode::KeyW | KeyCode::ArrowUp => self.up = pressed,
            KeyCode::KeyS | KeyCode::ArrowDown => self.down = pressed,
            KeyCode::KeyA | KeyCode::ArrowLeft => self.left = pressed,
            KeyCode::KeyD | KeyCode::ArrowRight => self.right = pressed,
            KeyCode::ShiftLeft => self.shift[0] = pressed,
            KeyCode::ShiftRight => self.shift[1] = pressed,
            _ => return false,
        }
        true
    }

    /// A field press changes the held state once. OS repeats must not restore a direction
    /// cleared by a story hold; releases always pass through, including after a blocked press.
    fn field_event(&mut self, code: KeyCode, pressed: bool, repeat: bool) -> bool {
        if pressed && repeat { return false; }
        self.set(code, pressed)
    }

    /// Is Shift held (walk instead of run)?
    pub fn walking(&self) -> bool {
        self.shift.iter().any(|&held| held)
    }

    /// Movement direction in area axes (+x right, +y up); opposite keys cancel.
    pub fn dir(&self) -> (f32, f32) {
        let axis = |pos: bool, neg: bool| f32::from(i8::from(pos) - i8::from(neg));
        (axis(self.right, self.left), axis(self.up, self.down))
    }
}

/// Physical keyboard movement keys held across a local modal boundary. Keep the aliases
/// separate: releasing Up must not release a still-held W. Keys used to navigate the panel
/// also need a release before they can move the player after it closes.
#[derive(Debug, Default)]
struct GraphicsKeyboard {
    held: u16,
    suppressed: u16,
    /// A release may have happened while the window could not receive it. A fresh physical
    /// press proves that release; an OS repeat or a focus-generated press does not.
    focus_uncertain: u16,
}

impl GraphicsKeyboard {
    fn bit(code: KeyCode) -> u16 {
        match code {
            KeyCode::KeyW => 1, KeyCode::ArrowUp => 2,
            KeyCode::KeyS => 4, KeyCode::ArrowDown => 8,
            KeyCode::KeyA => 16, KeyCode::ArrowLeft => 32,
            KeyCode::KeyD => 64, KeyCode::ArrowRight => 128,
            KeyCode::ShiftLeft => 256, KeyCode::ShiftRight => 512,
            _ => 0,
        }
    }

    /// Observe even consumed presses/releases. True means this press must not reach either
    /// a panel or gameplay until this physical key has been released.
    fn event(&mut self, code: KeyCode, pressed: bool, repeat: bool) -> bool {
        let bit = Self::bit(code);
        if pressed {
            if !repeat && self.focus_uncertain & bit != 0 {
                self.suppressed &= !bit;
                self.focus_uncertain &= !bit;
            }
            self.held |= bit;
            self.suppressed & bit != 0
        } else {
            self.held &= !bit;
            self.suppressed &= !bit;
            self.focus_uncertain &= !bit;
            false
        }
    }

    fn suppress_held(&mut self) { self.suppressed |= self.held; }

    fn focus_lost(&mut self) {
        self.suppress_held();
        self.focus_uncertain |= self.held;
    }

    /// Winit can synthesize releases on focus loss and presses on focus gain. Neither is
    /// evidence of a physical release; restored holds remain suppressed until real input.
    fn synthetic_event(&mut self, code: KeyCode, pressed: bool) {
        if pressed {
            let bit = Self::bit(code);
            self.held |= bit;
            self.suppressed |= bit;
            self.focus_uncertain |= bit;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GraphicsRoute { Pass, Consume, Open, Panel, Close }

/// A graphics key never falls through to movement, talking, battle or quit confirmation.
fn graphics_route(open: bool, available: bool, code: KeyCode, pressed: bool, repeat: bool) -> GraphicsRoute {
    if open {
        if !available || (code == KeyCode::F5 && pressed && !repeat) { GraphicsRoute::Close }
        else { GraphicsRoute::Panel }
    } else if code == KeyCode::F5 {
        if available && pressed && !repeat { GraphicsRoute::Open } else { GraphicsRoute::Consume }
    } else { GraphicsRoute::Pass }
}

fn graphics_available(state: &ClientState, talk_open: bool, now: f64) -> bool {
    !talk_open && !state.field_controls_held() && !state.scene.is_active()
        && !state.battle.in_battle() && !state.battle.lingering(now) && !state.ordinary_game_over()
}

/// A modal toggle/explicit close owns its whole controller batch. L3 sorts after A in the
/// pad's stable key order, so processing ordinary presses first could otherwise interact
/// with a character just before opening Graphics or change a setting just before closing it.
fn graphics_pad_priority(events: &[crate::gamepad::PadEvent], open: bool) -> Option<usize> {
    let toggle = events.iter().position(|event| event.pressed && !event.repeat && event.key.key_code() == KeyCode::F5);
    if toggle.is_some() { return toggle; }
    if open {
        events.iter().position(|event| event.pressed && !event.repeat
            && matches!(event.key.key_code(), KeyCode::Escape | KeyCode::Backspace))
    } else { None }
}

/// One line about a request the player just made (for stderr).
fn describe_request(cmd: PartyCommand) -> String {
    match cmd {
        PartyCommand::Invite(target) => format!("inviting the nearest player (entity {})", target.0),
        PartyCommand::Accept(invite) => format!("accepting {invite}"),
        PartyCommand::Leave => "leaving the party".to_string(),
        other => format!("{other:?}"),
    }
}

/// How fast the local player moves and which animation it announces, for the keys held: in an FF7
/// field (a [`Stage`]) it runs at the field's run speed, or walks at its walk speed while Shift is
/// held, never beyond the server's cap `max_speed`; anywhere else it keeps the old single pace
/// (`WALK_SPEED_FACTOR` of the cap, announced as walking).
pub fn movement_pace(stage: Option<&Stage>, walking: bool, max_speed: f32) -> (f32, AnimState) {
    match stage {
        Some(stage) if walking => (stage.walk_speed.min(max_speed), AnimState::Walk),
        Some(stage) => (stage.run_speed.min(max_speed), AnimState::Run),
        None => (WALK_SPEED_FACTOR * max_speed, AnimState::Walk),
    }
}

pub(crate) struct App {
    session: Session,
    /// How the area is drawn: walkmesh and bounds in view space, the mapping, the picture.
    /// Rebuilt when the server moves us to another area.
    view: FieldView,
    /// This machine's own FF7 data, to read the view of the next area from (`None`: areas are
    /// drawn from above).
    ff7: Option<Arc<Ff7Adapter>>,
    base_title: String,
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    input: Input,
    last_tick: Instant,
    last_title: Instant,
    /// What the title last said about the party: its size and the pending invite. A change shows
    /// at once instead of waiting for the next refresh.
    last_title_party: (usize, Option<fmv_protocol::InviteId>),
    /// What the title last said about the battle (empty outside one).
    last_title_battle: String,
    /// The 3D scene of the battle being fought (made when a battle is first drawn, dropped when
    /// the window is out of battles; `None` draws the flat view).
    battle_scene: Option<BattleScene>,
    /// The models the battle scenes use (this machine's own character archive), loaded on first use.
    battle_kit: Option<ModelKit>,
    left: bool,
    failure: Option<String>,
    /// The players' character models (FF7 fields with this machine's own `char.lgp` only).
    avatars: Option<Avatars>,
    /// The [`ClientState::npc_epoch`] the avatars were last told the NPCs of.
    npc_epoch: u64,
    /// F1: draw the walkmesh over the picture (a debugging aid, off by default).
    show_walkmesh: bool,
    /// Which part of the field the window shows (FF7's scrolling screen; F2: the whole field).
    camera: FieldCamera,
    /// Session time of the previous frame (for animation).
    last_draw_t: f64,
    /// Until when a first Escape press in the field keeps quitting armed (see [`escape_action`]).
    quit_armed_until: Option<Instant>,
    /// A short notice drawn over the frame, and when it goes away.
    notice: Option<(String, Instant)>,
    /// What the device asks of the window (full screen). The window is only created here on the
    /// command-line path; behind the character menu it was made by the menu, with the same value.
    device: DeviceMode,
    /// The game controllers. Closed until a window exists; the menu hands over the ones it opened.
    pads: Pads,
    /// Talking to the area's characters (`Enter` or `Space` next to one opens its dialogue window,
    /// which freezes the player while it is open).
    talk: Talk,
    /// The field's music, the battle music, the fanfare and the menu sounds (off unless
    /// [`App::with_audio`] gave it one: `--mute`, a library without sound and the fixture have none).
    audio: GameAudio,
    /// How many story scenes had begun when the window last looked (a new one lets go of the keys held and closes a talk).
    scene_started: u64,
    story_pending_epoch: u64,
    graphics: crate::graphics::Settings,
    graphics_panel: Option<GraphicsPanel>,
    graphics_keyboard: GraphicsKeyboard,
    focused: bool,
}

impl App {
    pub(crate) fn new(session: Session, view: FieldView) -> Self {
        let base_title = base_title(&session.info);
        let now = Instant::now();
        // Models only for real FF7 fields that have the camera data and this machine's archive.
        let mut avatars = if view.stage.is_some() && !session.info.fixture { Avatars::from_local_content() } else { None };
        if let Some(avatars) = avatars.as_mut() {
            // The first frame should not wait for the archive.
            avatars.preload(session.state.own_appearance());
        }
        Self {
            session,
            view,
            ff7: None,
            base_title,
            window: None,
            renderer: None,
            input: Input::default(),
            last_tick: now,
            last_title: now.checked_sub(TITLE_REFRESH).unwrap_or(now),
            last_title_party: (0, None),
            last_title_battle: String::new(),
            battle_scene: None,
            battle_kit: None,
            left: false,
            failure: None,
            avatars,
            npc_epoch: 0,
            show_walkmesh: false,
            camera: FieldCamera::default(),
            last_draw_t: 0.0,
            quit_armed_until: None,
            notice: None,
            device: DeviceMode::DESKTOP,
            pads: Pads::closed(),
            talk: Talk::ff7(None),
            audio: GameAudio::off(),
            scene_started: 0,
            story_pending_epoch: 0,
            graphics: crate::graphics::Settings::default(),
            graphics_panel: None,
            graphics_keyboard: GraphicsKeyboard::default(),
            focused: true,
        }
    }

    /// What the device asks of the window (see `crate::gamemode`).
    pub(crate) fn with_device(mut self, device: DeviceMode) -> Self {
        self.device = device;
        self
    }

    /// The sound of this run (made in `main`; the character menu hands over the one it was using).
    pub(crate) fn with_audio(mut self, audio: GameAudio) -> Self {
        self.audio = audio;
        self
    }

    /// Take over the controllers the character menu was already reading (the window and the
    /// renderer come over the same way, see [`App::adopt_surface`]).
    pub(crate) fn adopt_pads(&mut self, pads: Pads) {
        self.pads = pads;
    }

    /// A key went down, came up or repeated: from the keyboard or from a game controller, which
    /// make the same key events (`PadKey::key_code`) and so go through this one dispatch.
    fn key_event(&mut self, el: &ActiveEventLoop, code: KeyCode, pressed: bool, repeat: bool, source: InputSource) {
        let now = self.session.now();
        let suppressed = source == InputSource::Keyboard && self.graphics_keyboard.event(code, pressed, repeat);
        // Releases are never lost, even if a modal consumed the associated press.
        if !pressed { self.input.set(code, false); }
        let was_open = self.graphics_panel.is_some();
        self.sync_story_pending_input();
        self.sync_graphics_panel();
        if suppressed || !self.focused || (was_open && self.graphics_panel.is_none()) { return; }
        let available = self.focused && self.renderer.is_some() && graphics_available(&self.session.state, self.talk.is_open(), now);
        match graphics_route(self.graphics_panel.is_some(), available, code, pressed, repeat) {
            GraphicsRoute::Open => {
                self.capture_graphics_input();
                self.notice = None;
                self.graphics_panel = Some(GraphicsPanel::new());
                return;
            }
            GraphicsRoute::Close => { self.close_graphics_panel(); return; }
            GraphicsRoute::Consume => return,
            GraphicsRoute::Panel => {
                let action = self.graphics_panel.as_mut().and_then(|panel| panel.key(code, pressed, repeat));
                self.graphics_action(action);
                return;
            }
            GraphicsRoute::Pass => {}
        }
        match game_over_route(&self.session.state, code, pressed, repeat) {
            GameOverRoute::Disconnect => { self.quit(el); return; }
            GameOverRoute::Held => {
                self.input = Input::default();
                self.talk.close();
                return;
            }
            GameOverRoute::Inactive => {}
        }
        let route = if self.session.state.field_controls_held() {
            self.talk.close();
            // Key releases must survive every hold, even when the associated press was ignored.
            if !pressed { self.input.set(code, false); }
            let battle_menu = self.session.state.battle.in_battle() || self.session.state.battle.lingering(now);
            story_pending_route(code, pressed, repeat, battle_menu)
        } else {
            let area = &self.session.info.area_id.key;
            match scene::route_key(&mut self.session.state, area, now, self.talk.is_open(), code, pressed, repeat) {
                SceneRoute::Scene => {
                    // A key let go of during a scene still ends the walk: it must never be lost.
                    if !pressed {
                        self.input.set(code, false);
                    }
                    return;
                }
                SceneRoute::Preview => {
                    eprintln!("scene: asking the server for a preview of the first scene");
                    self.notice = Some(("Asking the server for a scene preview".to_string(), Instant::now() + NOTICE_TIME));
                    return;
                }
                SceneRoute::Press(npc) => {
                    eprintln!("scene: OK next to a scene's trigger (character {npc})");
                    self.session.send_interact(Some(npc));
                    return;
                }
                SceneRoute::Field => {}
            }
            // A dialogue takes its keys first; a fresh Enter or Space outside a battle talks to the
            // character in front of us. It is also a press of OK, which the server must hear of (a character's talk can
            // move the story; a ladder or a lift panel is used by it): the conversation it opens is built first, from the
            // story as it is now, and the press goes out after it (see `field_key`).
            let key = field_key(&mut self.talk, &self.session.state, &self.session.info.area_id.key, self.session.now(), code, pressed, repeat);
            if let Some(npc) = key.interact {
                self.session.send_interact(npc);
            }
            if key.consumed {
                return;
            }
            route_key(code, pressed, repeat, self.session.state.battle.in_battle())
        };
        if self.talk.is_open() && route != KeyRoute::Movement {
            return; // under a dialogue only the walking keys count (a release must never be lost)
        }
        match route {
            KeyRoute::Escape => self.escape(el, source),
            KeyRoute::WalkMesh => self.show_walkmesh = !self.show_walkmesh,
            KeyRoute::Camera => {
                let text = match self.camera.toggle() {
                    CameraMode::Follow => "Camera: follows you, as in FF7 (F2: the whole field)",
                    CameraMode::Whole => "Camera: the whole field (F2: follow you, as in FF7)",
                };
                self.notice = Some((text.to_string(), Instant::now() + NOTICE_TIME));
            }
            KeyRoute::AntiAliasing => {
                if let Some(renderer) = self.renderer.as_mut() {
                    let text = renderer.toggle_anti_aliasing();
                    self.notice = Some((text.to_string(), Instant::now() + NOTICE_TIME));
                }
            }
            KeyRoute::RenderScale => {
                if let Some(renderer) = self.renderer.as_mut() {
                    let text = renderer.toggle_render_scale();
                    self.notice = Some((text, Instant::now() + NOTICE_TIME));
                }
            }
            KeyRoute::Party(key) => self.party_key(key),
            KeyRoute::Audio(key) => self.audio_key(key),
            KeyRoute::Battle(key) => self.battle_key(key),
            KeyRoute::Swap => self.swap_key(),
            KeyRoute::Movement => {
                self.input.field_event(code, pressed, repeat);
            }
            KeyRoute::Ignored => {}
        }
    }

    fn sync_story_pending_input(&mut self) {
        if observe_story_pending(&mut self.story_pending_epoch, self.session.state.story_pending_epoch(), &mut self.input) {
            self.talk.close();
            self.close_graphics_panel();
        }
    }

    fn capture_graphics_input(&mut self) {
        self.input = Input::default();
        self.graphics_keyboard.suppress_held();
        self.pads.suppress_directions_until_release();
        self.quit_armed_until = None;
    }

    fn close_graphics_panel(&mut self) {
        if self.graphics_panel.take().is_some() { self.capture_graphics_input(); }
    }

    fn sync_graphics_panel(&mut self) {
        if !self.focused || !graphics_available(&self.session.state, self.talk.is_open(), self.session.now()) {
            self.close_graphics_panel();
        }
    }

    fn graphics_action(&mut self, action: Option<GraphicsAction>) {
        let Some(action) = action else { return };
        if action == GraphicsAction::Close { self.close_graphics_panel(); return; }
        let Some(renderer) = self.renderer.as_mut() else { return };
        let notice = match action {
            GraphicsAction::AntiAliasing => renderer.toggle_anti_aliasing().to_string(),
            GraphicsAction::RenderScale => renderer.toggle_render_scale(),
            GraphicsAction::BackgroundFilter => renderer.toggle_background_filter().to_string(),
            GraphicsAction::Defaults => renderer.restore_graphics_defaults(),
            GraphicsAction::Close => return,
        };
        if let Some(panel) = self.graphics_panel.as_mut() { panel.set_notice(notice); }
    }

    /// Read the game controllers and send what they did through [`App::key_event`]. In a battle (or
    /// while its panel lingers) the stick reads in four directions and repeats; in the field it
    /// reads in eight and a held direction is a held key.
    fn poll_pads(&mut self, el: &ActiveEventLoop, now: Instant) {
        let battle = &self.session.state.battle;
        // A save hold alone keeps Field mode: toggling modes would synthesize fresh presses
        // for a still-held diagonal stick when the save completes.
        let menu = self.graphics_panel.is_some() || battle.in_battle() || battle.lingering(self.session.now()) || self.session.state.ordinary_game_over() || self.talk.is_open() || self.session.state.scene.is_active();
        let mode = if menu { PadMode::Menu } else { PadMode::Field };
        let events = self.pads.poll(now, mode);
        let priority = graphics_pad_priority(&events, self.graphics_panel.is_some());
        if let Some(index) = priority {
            let event = &events[index];
            let panel_open = self.graphics_panel.is_some();
            self.key_event(el, event.key.key_code(), event.pressed, event.repeat, InputSource::Pad);
            if panel_open != self.graphics_panel.is_some() { return; }
        }
        for (index, event) in events.into_iter().enumerate() {
            if priority == Some(index) { continue; }
            let panel_open = self.graphics_panel.is_some();
            self.key_event(el, event.key.key_code(), event.pressed, event.repeat, InputSource::Pad);
            // Remaining events were collected under the previous modal focus. In particular,
            // a held navigation repeat or another button from this batch must not leak into
            // gameplay after B/L3 closes the panel. Boundary capture already cleared walking.
            if panel_open != self.graphics_panel.is_some() { break; }
        }
    }

    /// Escape was pressed: the battle menu goes back a level first (or the panel after a battle
    /// closes); otherwise quit only on a second press in the field, never in a battle.
    fn escape(&mut self, el: &ActiveEventLoop, source: InputSource) {
        let now = Instant::now();
        let (session_now, armed) = (self.session.now(), quit_armed(self.quit_armed_until, now));
        match escape_route(&mut self.session.state, session_now, armed) {
            // The menu went back a level (or the panel after the battle closed).
            EscapeRoute::Battle => self.audio.cue(Cue::Cancel),
            EscapeRoute::Action(EscapeAction::IgnoreInBattle) => {
                match source {
                    InputSource::Keyboard => eprintln!("battle: Esc does not quit during a battle (close the window to quit)"),
                    InputSource::Pad => eprintln!("battle: Start does not quit during a battle"),
                }
                self.notice = escape_notice(EscapeAction::IgnoreInBattle, source).map(|text| (text.to_string(), now + NOTICE_TIME));
            }
            EscapeRoute::Action(EscapeAction::Arm) => {
                self.quit_armed_until = Some(now + QUIT_CONFIRM);
                self.notice = escape_notice(EscapeAction::Arm, source).map(|text| (text.to_string(), now + QUIT_CONFIRM));
            }
            EscapeRoute::Action(EscapeAction::Quit) => self.quit(el),
        }
    }

    /// The notice to draw now, if one is up.
    fn current_notice(&self) -> Option<&str> {
        self.notice.as_ref().filter(|(_, until)| Instant::now() < *until).map(|(text, _)| text.as_str())
    }

    /// Where the views of later areas come from: when the server moves the player to another
    /// area, its view (camera, background, walkmesh) is built from this machine's own FF7 data.
    pub(crate) fn with_ff7(mut self, ff7: Option<Arc<Ff7Adapter>>) -> Self {
        self.talk = Talk::ff7(ff7.clone());
        // Story scenes are built from the same data (without it a scene cannot be played here and its player releases itself).
        self.session.state.scene.set_builder(Box::new(scene::Ff7Scenes::new(ff7.clone())));
        self.ff7 = ff7;
        self
    }

    /// The session entered another area: draw it (a new view, a new title).
    fn rebuild_view(&mut self) {
        self.close_graphics_panel();
        self.talk.close();
        self.view = crate::field_view(self.ff7.as_deref(), &self.session);
        if let Some(renderer) = self.renderer.as_ref() {
            if let Err(error) = self.view.set_background_texture_limit(renderer.max_texture_dimension_2d()) {
                eprintln!("field background texture eligibility: {error}");
            }
        }
        // A new field: the view is put on the player at once, not scrolled there from the last field's place.
        self.camera.snap();
        self.base_title = base_title(&self.session.info);
        // The next `update_title` must not wait for its refresh interval.
        self.last_title = Instant::now().checked_sub(TITLE_REFRESH).unwrap_or_else(Instant::now);
    }

    /// Take over a window and renderer that already exist (the character menu opens them first
    /// and hands them over when the player joins). The window gets the game's title on the next
    /// tick.
    pub(crate) fn adopt_surface(&mut self, window: Arc<Window>, renderer: Renderer) {
        if let Err(error) = self.view.set_background_texture_limit(renderer.max_texture_dimension_2d()) {
            eprintln!("field background texture eligibility: {error}");
        }
        self.window = Some(window);
        self.renderer = Some(renderer);
        self.last_tick = Instant::now();
    }

    /// The run's exit code once the event loop ended with `result`: says goodbye first (a no-op
    /// when the loop already did) and reports a failure or a crash on stderr.
    pub(crate) fn finish(&mut self, result: LoopResult) -> u8 {
        self.leave();
        match (self.failure.take(), describe_loop_failure(&result)) {
            (Some(msg), _) | (None, Some(msg)) => {
                eprintln!("error: {msg}");
                exit::FAILURE
            }
            (None, None) => exit::OK,
        }
    }

    /// Send `Leave` once and wait (bounded) for it to be flushed.
    fn leave(&mut self) {
        if !self.left {
            self.left = true;
            self.session.leave(LEAVE_WAIT);
        }
    }

    fn quit(&mut self, el: &ActiveEventLoop) {
        self.leave();
        el.exit();
    }

    fn fail(&mut self, el: &ActiveEventLoop, msg: String) {
        if self.failure.is_none() {
            self.failure = Some(msg);
        }
        self.leave();
        el.exit();
    }

    fn update_title(&mut self) {
        let Some(window) = &self.window else { return };
        let party = &self.session.state.party;
        let invite = party.newest_invite_to_me();
        let party_key = (party.party_size(), invite.as_ref().map(|i| i.id));
        let battle_text = self.session.state.battle.title_suffix(self.session.now());
        if self.last_title.elapsed() < TITLE_REFRESH && party_key == self.last_title_party && battle_text == self.last_title_battle {
            return;
        }
        self.last_title = Instant::now();
        self.last_title_party = party_key;
        self.last_title_battle = battle_text.clone();
        let rtt = self
            .session
            .state
            .last_rtt()
            .map_or_else(|| "-".to_string(), |d| format!("{} ms", d.as_millis()));
        let title = live_title(
            &self.base_title,
            self.session.state.remote_count() + 1,
            &rtt,
            party_key.0,
            invite.as_ref().map(|i| i.from_name.as_str()),
        );
        window.set_title(&format!("{title}{battle_text}"));
    }

    /// A sound key was pressed: mute, or change the volume. The notice says what happened (and, in a run
    /// that has no sound, that there is none).
    fn audio_key(&mut self, key: AudioKey) {
        let said = match key {
            AudioKey::Mute => self.audio.toggle_mute().map(|muted| if muted { "Sound off (M turns it on)" } else { "Sound on" }),
            AudioKey::VolumeDown => self.audio.change_volume(false).map(|_| "Volume down"),
            AudioKey::VolumeUp => self.audio.change_volume(true).map(|_| "Volume up"),
        };
        let text = said.unwrap_or("This run has no sound (see --help, SOUND)");
        self.notice = Some((text.to_string(), Instant::now() + NOTICE_TIME));
    }

    /// A battle key was pressed: send the request it stands for, play the menu sound it made, and say what
    /// it did on stderr.
    fn battle_key(&mut self, key: BattleKey) {
        let now = self.session.now();
        let request = battle_request(key, &mut self.session.state, now);
        if let Some(cue) = battle_cue(key, &request) {
            self.audio.cue(cue);
        }
        match request {
            BattleRequest::Send(cmd) => {
                eprintln!(
                    "battle: {}",
                    match cmd {
                        BattleCmd::RequestEncounter { .. } => "asking the server for a debug battle".to_string(),
                        // Debug text, so a command kind the engine gains later needs no change here.
                        BattleCmd::Command { actor, command, .. } => format!("actor {} sends {command:?}", actor.0),
                    }
                );
                self.session.send_battle(cmd);
            }
            BattleRequest::Menu | BattleRequest::Nothing("") => {}
            BattleRequest::Nothing(why) => eprintln!("battle: {why}"),
        }
    }

    /// `Tab` was pressed in a battle: the menu goes to the next ready member of the party the player commands, with the
    /// cursor sound (nothing happens, silently, when only one is waiting or the menu is not at its command list).
    fn swap_key(&mut self) {
        if self.session.state.battle.swap_turn() == MenuReply::Handled {
            self.audio.cue(Cue::Cursor);
        }
    }

    /// A party key was pressed: send the request it stands for, and say what it did on stderr
    /// (the window shows text only in its title).
    fn party_key(&mut self, key: PartyKey) {
        let render_t = render_time(self.session.now());
        match party_request(key, &self.session.state, render_t) {
            Some(cmd) => {
                eprintln!("party: {}", describe_request(cmd));
                self.session.send_party(cmd);
            }
            None => eprintln!(
                "party: {}",
                match key {
                    PartyKey::Invite => "nobody to invite nearby",
                    PartyKey::Accept => "no invite to accept",
                    PartyKey::Leave => "nothing to leave",
                }
            ),
        }
    }

    /// The server refused a scene request (the F6 preview on a server that does not run it, an OK that cannot start one now):
    /// one line on the screen and on stderr, as the other refusals are shown.
    fn scene_notice(&mut self) {
        if let Some(reason) = self.session.state.scene.take_refusal() {
            eprintln!("scene: the server refused it ({reason:?})");
            self.notice = Some((refusal_notice(reason).to_string(), Instant::now() + NOTICE_TIME));
        }
    }

    fn tick(&mut self, el: &ActiveEventLoop) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f32().min(MAX_DT);
        self.last_tick = now;
        // Controllers first, before anything that can return early (a battle freezes the walk, not
        // the menu): a press of Start may have just said goodbye, and then there is nothing to tick.
        self.poll_pads(el, now);
        if self.left {
            return;
        }
        if let Some(reason) = self.session.pump() {
            self.fail(el, reason);
            return;
        }
        self.sync_story_pending_input();
        // The server moved us to another area: draw it before anything is drawn again.
        if self.session.take_area_change().is_some() {
            self.rebuild_view();
        }
        // A story scene: its clock, what it owes the server, what it has to say. A new one lets go of the keys held when it
        // began (their releases may be swallowed) and closes a talk (the server holds the player).
        let scene_visible = !self.session.state.field_controls_held() && !self.session.state.battle.in_battle() && !self.session.state.battle.lingering(self.session.now());
        self.session.state.scene.advance_visible(dt, scene_visible);
        self.session.flush_scene();
        self.session.log_scene_notes();
        self.scene_notice();
        if self.session.state.scene.started() != self.scene_started {
            self.scene_started = self.session.state.scene.started();
            self.input = Input::default();
            self.talk.close();
            self.close_graphics_panel();
        }
        self.sync_graphics_panel();
        // The music follows the area and the battle (before the battle's early return below: a fight is
        // exactly when it changes).
        self.audio.update_from(&self.session);
        let now = self.session.now();
        self.audio.follow_scene(&mut self.session.state.scene, now);
        self.session.log_party_notes();
        self.session.log_battle_notes();
        self.session.flush_party_sync();
        // A player in a battle is frozen in the field: no walking, no movement intents. So is one whose
        // battle just ended, while the window still shows its victory or defeat panel.
        if self.session.state.field_controls_held() || self.session.state.battle.in_battle() || self.session.state.battle.lingering(self.session.now()) {
            self.talk.close();
            self.update_title();
            return;
        }
        let (speed, moving) = movement_pace(self.view.stage.as_ref(), self.input.walking(), self.session.area.max_speed());
        // The keys mean directions on the SCREEN; the area is walked in its own axes. Under a dialogue
        // window the player stands still (no keys: idle, and the intent is sent as usual).
        let keys = if self.graphics_panel.is_some() || self.talk.is_open() || self.session.state.scene.is_active() { (0.0, 0.0) } else { self.input.dir() };
        let dir = if keys == (0.0, 0.0) {
            keys
        } else {
            self.view
                .screen_to_world_dir(self.session.state.own.pos(), keys)
                .unwrap_or(keys)
        };
        self.session
            .state
            .own
            .walk_as(self.session.area.as_ref(), dir, speed, dt, moving);
        self.session.flush_intent();
        self.update_title();
    }

    fn redraw(&mut self, el: &ActiveEventLoop) {
        let notice = self.current_notice().map(str::to_owned);
        let Some(renderer) = self.renderer.as_mut() else { return };
        let now = self.session.now();
        let render_t = render_time(now);
        let dt = (now - self.last_draw_t).clamp(0.0, f64::from(MAX_DT)) as f32;
        self.last_draw_t = now;
        // In a battle the whole window is the battle: the 3D scene when it can be had, else the flat
        // tokens of the battle view, with the battle overlay (the windows, the numbers, the status
        // band) over it: over the scene's own fighters' heads, or over the tokens'.
        if let Some(battle) = self.session.state.battle.view(now) {
            let (vw, vh) = renderer.size();
            let mut overlay = DrawList::new();
            // What the last messages did is taken every frame, drawn as a scene or not (the view
            // lingers after the battle's end, and the last events are played with it).
            let events = self.session.state.battle.take_events();
            let drawn = scene_for(&mut self.battle_scene, &mut self.battle_kit, &self.session.state, &battle, now, &events).map(|scene| {
                let view = scene.view(vw, vh);
                draw_battle_overlay(&battle, &scene.head_anchors(&view), vw, vh, &mut overlay);
                (scene, view)
            });
            if drawn.is_none() {
                draw_battle_text(&battle, vw, vh, &mut overlay);
            }
            if let Some(text) = notice.as_deref() {
                draw_notice(text, vw, vh, &mut overlay);
            }
            if self.session.state.ordinary_game_over() {
                draw_game_over(vw, vh, &mut overlay);
            }
            let result = match drawn {
                Some((scene, view)) => renderer.render_battle(|_, _, srgb| scene.frame(&view, srgb), Some(&overlay)),
                None => renderer.render_flat(|w, h, srgb| battle_vertices(&battle, w, h, srgb), Some(&overlay)),
            };
            let gpu_error = renderer.first_gpu_error();
            match (result, gpu_error) {
                (FrameResult::Fatal(msg), _) => self.fail(el, msg),
                (_, Some(e)) => self.fail(el, format!("GPU error: {e}")),
                _ => {}
            }
            return;
        }
        // Out of battles: whatever scene there was is over.
        self.battle_scene = None;
        self.view.apply_background_events(self.session.state.scene.drain_background());
        let draw = field_draw(self.avatars.as_mut(), &mut self.npc_epoch, &self.view, &self.session.state, self.session.info.tint, render_t, dt);
        // The part of the field the window shows this frame: around the player's character as it is drawn now, or
        // where the scene points the camera (everything below maps view space to the window through it).
        let (vw, vh) = renderer.size();
        self.view.bounds = self.camera.update(&camera_input(&self.view, &self.session.state, &draw, (vw, vh), dt));
        let view = &self.view;
        let state = &self.session.state;
        let (built, markers) = (draw.built, draw.markers);

        // The walkmesh is the scenery of an area without a picture; over a picture it is a
        // debugging aid, shown on request (F1).
        let walkmesh = view.backdrop.is_none() || self.show_walkmesh;
        let scene = Scene {
            bounds: view.bounds,
            triangles: if walkmesh { &view.triangles } else { &[] },
            players: &markers,
            fixture_banner: self.session.info.fixture,
            backdrop: view.backdrop.as_ref(),
        };
        // Names, party, area, connection (and the FIXTURE label) over the scene; a name stays over
        // the head of a model instead of over the marker's centre.
        let mut overlay = DrawList::new();
        // (a scene's trigger is not drawn while its scene runs, and is not named either)
        let story = state.scene.player();
        let anchor = |subject: Subject, at: Pos| {
            if scene::label_hidden(story, subject, state.npc_views())
                || scene::participant_label_hidden(story, state.self_entity(), subject) {
                None
            } else {
                label_anchor(built.as_ref(), view, draw.own_who, subject, at)
            }
        };
        let mut hud_input = hud::gather_anchored(state, &self.session.info, view, render_t, &anchor);
        if let Some(story) = story {
            // Cast names replace the hidden field NPC labels and move with the drawn actors.
            // Existing player labels follow them in draw order, so a player's name stays on top.
            let mut labels = scene::cast_labels(story, built.as_ref(), view);
            labels.append(&mut hud_input.labels);
            hud_input.labels = labels;
        }
        hud::draw_hud_with(&hud_input, hud::progress_line(state, render_t).as_ref(), vw, vh, &mut overlay);
        crate::ui::countdown::draw(&state.countdown, vw, vh, self.session.info.fixture, now, &mut overlay);
        self.talk.draw(vw, vh, now, &mut overlay);
        // The window of a story scene, near whoever speaks.
        if let Some(story) = story {
            let spot = scene::speaker_spot(story, built.as_ref(), view, vw, vh);
            story.draw(vw, vh, now, spot.as_ref(), &mut overlay);
        }
        if let Some(text) = notice.as_deref() {
            draw_notice(text, vw, vh, &mut overlay);
        }
        if state.ordinary_game_over() {
            draw_game_over(vw, vh, &mut overlay);
        }
        if let Some(panel) = &self.graphics_panel {
            panel.draw(renderer.graphics_status(), (vw, vh), &mut overlay);
        }
        let result = renderer.render_layers_effects(&scene, built.as_ref().map(|b| &b.frame), Some(&overlay), self.camera.clip_range(view.range), state.scene.fade());
        let gpu_error = renderer.first_gpu_error();
        match (result, gpu_error) {
            (FrameResult::Fatal(msg), _) => self.fail(el, msg),
            (_, Some(e)) => self.fail(el, format!("GPU error: {e}")),
            _ => {}
        }
    }
}

/// What the field camera ([`crate::camera`]) needs to know about a frame of the field: the field's range, the player's
/// character as `draw` drew it, what a running story scene asks, and the shape of a `vw` x `vh` window. The window and the
/// offscreen pictures both build it here.
pub(crate) fn camera_input(view: &FieldView, state: &ClientState, draw: &FieldDraw, (vw, vh): (u32, u32), dt: f32) -> CameraInput {
    CameraInput {
        range: view.range,
        whole: view.whole,
        window: [vw as f32, vh as f32],
        margin: crate::render::MARGIN_PX,
        own: draw.own_at.map(|p| [p.x, p.y]),
        scene: state.scene.player().map(|story| scene::camera_shot(story, view)),
        shake: state.scene.player().map_or([0.0; 2], |story| story.shake()),
        dt,
    }
}

/// What the screen says when the server refuses a scene request.
fn refusal_notice(reason: fmv_protocol::SceneRefusal) -> &'static str {
    use fmv_protocol::SceneRefusal;
    match reason {
        SceneRefusal::Unavailable => "This server does not run scene previews",
        SceneRefusal::UnknownScene => "The server has no such scene",
        SceneRefusal::Busy => "A scene cannot start now",
        SceneRefusal::NotHere => "That scene is not on offer here",
    }
}

/// What a frame of the field shows apart from the picture and the overlay: the models that were
/// built and the markers for everyone who has none.
pub(crate) struct FieldDraw {
    /// The avatars' frame, `None` when this window draws markers only (no models here, or a field
    /// without a camera).
    pub built: Option<Built>,
    /// Markers for whoever is not drawn as a model, the area's characters first and the local
    /// player last, so that every player is drawn over the characters.
    pub markers: Vec<PlayerSprite>,
    /// The key of the local player in `built`.
    pub own_who: Who,
    /// Where the local player's character is drawn this frame (view space; on its witness spot while a scene runs), for
    /// the field camera to centre on. `None` when it cannot be drawn.
    pub own_at: Option<Pos>,
}

/// Everyone who is drawn in this frame of the field: the area's non-player characters first, then
/// the remote players, then the local one last (so every player is drawn over the characters, the
/// local one over everyone). Models where the character is available, markers for everyone else
/// (the characters of the area included: a model that is not to be had here is a marker with a
/// name). One that cannot be placed (behind the camera) is simply not shown.
///
/// When the area's characters changed since the last call (`npc_epoch` tells), the models they
/// name are loaded now and the last area's are dropped, so the first frame that shows them does
/// not stall. The window and the offscreen shots both draw through here.
pub(crate) fn field_draw(
    avatars: Option<&mut Avatars>,
    npc_epoch: &mut u64,
    view: &FieldView,
    state: &ClientState,
    own_tint: [u8; 3],
    render_t: f64,
    dt: f32,
) -> FieldDraw {
    let mut avatars = avatars;
    if *npc_epoch != state.npc_epoch() {
        *npc_epoch = state.npc_epoch();
        if let Some(avatars) = avatars.as_deref_mut() {
            avatars.set_npcs(&npc_models(state.npc_views()));
        }
    }
    let remotes = state.remote_views(render_t);
    // While a story scene runs the cast stands among the area's characters (its trigger is not drawn: the cast member that
    // plays its part is) and the players that take part are drawn on their witness spots (see `crate::scene::draw`).
    let story = state.scene.player();
    let npcs = scene::visible_npcs(state.npc_views(), story);
    let mut players: Vec<AvatarPlayer<'_>> = npc_players(&npcs);
    if let Some(story) = story {
        players.extend(scene::cast_players(story));
    }
    players.extend(remotes.iter().map(|v| AvatarPlayer {
        who: Who(v.id.0),
        pos: v.pose.pos,
        facing: v.pose.facing,
        anim: v.pose.anim,
        scene_frame: None, scene_clip: None, scene_return: None,
        appearance: v.appearance.as_deref(),
        npc: None,
        tint: v.tint,
        own: false,
        party: state.party.is_party_entity(v.id),
    }));
    let own_who = Who(state.self_entity().0);
    players.push(AvatarPlayer {
        who: own_who,
        pos: state.own.pos(),
        facing: state.own.facing(),
        anim: state.own.anim(),
        scene_frame: None, scene_clip: None, scene_return: None,
        appearance: state.own_appearance(),
        npc: None,
        tint: own_tint,
        own: true,
        party: state.party.party_size() > 0,
    });
    if let Some(story) = story {
        scene::place_witnesses(&mut players, story);
        // Apply visibility before both model and fallback-marker construction. Cast ids occupy
        // their own namespace; only actual participants named by the scene can be hidden.
        players.retain(|p| !story.hides_participant(fmv_core::EntityId(p.who.0)));
    }
    let own_at = players.iter().find(|p| p.own).and_then(|p| view.to_view(p.pos));
    let built = match (avatars, view.stage.as_ref()) {
        (Some(avatars), Some(stage)) => Some(avatars.frame(view, stage, &players, dt)),
        _ => None,
    };
    let markers = players
        .iter()
        .enumerate()
        .filter(|(i, _)| built.as_ref().is_none_or(|b| b.markers.contains(i)))
        .filter_map(|(_, p)| marker(view, p))
        .collect();
    FieldDraw { built, markers, own_who, own_at }
}

/// The marker of a player or of a non-player character: where its feet are in view space, in its
/// tint. `None` if it cannot be drawn (behind the camera).
fn marker(view: &FieldView, player: &AvatarPlayer<'_>) -> Option<PlayerSprite> {
    view.to_view(player.pos).map(|p| PlayerSprite { x: p.x, y: p.y, tint: player.tint, own: player.own, party: player.party })
}

/// Where the name of `subject` goes, in view space: over the head of its model when it was drawn
/// as one (`built` says), else at its marker (`at` is where it stands).
pub(crate) fn label_anchor(built: Option<&Built>, view: &FieldView, own: Who, subject: Subject, at: Pos) -> Option<Pos> {
    let who = match subject {
        Subject::Own => own,
        Subject::Remote(id) => Who(id.0),
        Subject::Npc(id) => npc_who(id),
    };
    built.and_then(|b| b.anchors.get(&who).copied()).or_else(|| view.to_view(at))
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = window_attributes(self.base_title.clone(), self.device);
        let window = match el.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => return self.fail(el, format!("cannot create the window: {e}")),
        };
        match Renderer::new(Arc::clone(&window), el.owned_display_handle()) {
            Ok(mut r) => {
                r.set_graphics(std::mem::take(&mut self.graphics));
                eprintln!("renderer: {}", r.describe());
                if let Err(error) = self.view.set_background_texture_limit(r.max_texture_dimension_2d()) {
                    eprintln!("field background texture eligibility: {error}");
                }
                self.renderer = Some(r);
            }
            Err(e) => return self.fail(el, format!("cannot start the renderer: {e}")),
        }
        self.window = Some(window);
        self.pads = Pads::open();
        self.last_tick = Instant::now();
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => self.quit(el),
            WindowEvent::Resized(size) => {
                if let Some(r) = self.renderer.as_mut() {
                    r.resize(size.width, size.height);
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        physical_key: PhysicalKey::Code(code),
                        state,
                        repeat,
                        ..
                    },
                is_synthetic,
                ..
            } => {
                let pressed = state == ElementState::Pressed;
                if is_synthetic {
                    self.graphics_keyboard.synthetic_event(code, pressed);
                    if !pressed { self.input.set(code, false); }
                    return;
                }
                self.key_event(el, code, pressed, repeat, InputSource::Keyboard);
            }
            WindowEvent::Focused(focused) => {
                self.focused = focused;
                // Controllers are only heard while the window has the focus (see `crate::gamepad`).
                self.pads.set_focused(focused);
                // Releases while unfocused are never delivered: do not keep walking.
                if !focused {
                    self.close_graphics_panel();
                    self.graphics_keyboard.focus_lost();
                    self.input = Input::default();
                }
            }
            WindowEvent::RedrawRequested => self.redraw(el),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        if self.window.is_none() {
            return;
        }
        self.tick(el);
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        el.set_control_flow(ControlFlow::WaitUntil(Instant::now() + FRAME));
    }

    fn exiting(&mut self, _el: &ActiveEventLoop) {
        self.leave();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_core::{AccountId, AnimState, AreaId, AvatarAppearance, EntityId, EntitySnapshot, GameId, Pos};
    use fmv_protocol::{InviteId, PartyEvent, PartyId, PartyName, ServerMsg};

    #[test]
    fn a_story_save_hold_blocks_field_actions_but_keeps_battle_and_local_controls() {
        for key in [KeyCode::KeyW, KeyCode::ArrowUp, KeyCode::Enter, KeyCode::Space,
            KeyCode::F5, KeyCode::F6, KeyCode::KeyB, KeyCode::KeyP, KeyCode::KeyY, KeyCode::KeyL] {
            assert_eq!(story_pending_route(key, true, false, false), KeyRoute::Ignored, "{key:?} cannot begin a field action");
        }
        for key in [KeyCode::Escape, KeyCode::F1, KeyCode::F2, KeyCode::F3, KeyCode::F4,
            KeyCode::KeyM, KeyCode::Minus, KeyCode::Equal] {
            assert_eq!(story_pending_route(key, true, false, false), route_key(key, true, false, false), "{key:?} remains local");
        }
        for (key, expected) in [(KeyCode::Enter, KeyRoute::Battle(BattleKey::Confirm)),
            (KeyCode::Backspace, KeyRoute::Battle(BattleKey::Back)),
            (KeyCode::ArrowUp, KeyRoute::Battle(BattleKey::Up)),
            (KeyCode::Tab, KeyRoute::Swap), (KeyCode::KeyY, KeyRoute::Swap)] {
            assert_eq!(story_pending_route(key, true, false, true), expected, "a held story may already be fighting or showing its result panel");
        }
        assert_eq!(story_pending_route(KeyCode::Escape, true, false, true), KeyRoute::Escape);
        assert_eq!(story_pending_route(KeyCode::KeyB, true, false, true), KeyRoute::Ignored, "a hold never asks for a second battle");
        assert_eq!(story_pending_route(KeyCode::KeyL, true, false, true), KeyRoute::Ignored, "battle input does not allow a party mutation");
    }

    #[test]
    fn graphics_focus_consumes_keys_before_talking_battles_or_quit_confirmation() {
        assert_eq!(graphics_route(false, true, KeyCode::F5, true, false), GraphicsRoute::Open);
        for (pressed, repeat) in [(false, false), (true, true)] {
            assert_eq!(graphics_route(false, true, KeyCode::F5, pressed, repeat), GraphicsRoute::Consume);
        }
        assert_eq!(graphics_route(false, false, KeyCode::F5, true, false), GraphicsRoute::Consume);
        assert_eq!(route_key(KeyCode::F5, true, false, false), KeyRoute::Ignored);
        assert_eq!(graphics_route(true, true, KeyCode::F5, true, false), GraphicsRoute::Close);
        for code in [KeyCode::Enter, KeyCode::Space, KeyCode::Escape, KeyCode::Backspace,
            KeyCode::KeyW, KeyCode::KeyP, KeyCode::KeyB, KeyCode::F6] {
            assert_eq!(graphics_route(true, true, code, true, false), GraphicsRoute::Panel,
                "{code:?} never reaches an NPC, battle, preview or quit route behind the overlay");
            assert_eq!(graphics_route(true, false, code, true, false), GraphicsRoute::Close,
                "a new gameplay modal dismisses graphics and consumes the crossing event");
        }
        let mut panel = GraphicsPanel::new();
        assert_eq!(panel.key(KeyCode::Escape, true, false), Some(GraphicsAction::Close));
        assert_eq!(panel.key(KeyCode::Backspace, true, false), Some(GraphicsAction::Close));
        assert_eq!(graphics_route(false, true, KeyCode::Enter, true, false), GraphicsRoute::Pass,
            "normal gameplay dispatch returns after the overlay has closed");
    }

    #[test]
    fn graphics_keyboard_requires_release_for_old_walking_and_panel_navigation_keys() {
        let mut keyboard = GraphicsKeyboard::default();
        for code in [KeyCode::KeyW, KeyCode::ArrowUp, KeyCode::ShiftLeft] {
            assert!(!keyboard.event(code, true, false));
        }
        keyboard.suppress_held(); // Opening the panel retires the old movement.
        assert!(keyboard.event(KeyCode::KeyW, true, false));
        assert!(!keyboard.event(KeyCode::ArrowUp, false, false));
        assert!(keyboard.event(KeyCode::KeyW, true, false), "the Up alias does not release W");
        assert!(!keyboard.event(KeyCode::ArrowUp, true, false), "released input may navigate again");
        assert!(!keyboard.event(KeyCode::ArrowRight, true, false), "a fresh panel navigation key");
        keyboard.suppress_held(); // Closing it must not turn held navigation into walking.
        for code in [KeyCode::KeyW, KeyCode::ArrowUp, KeyCode::ArrowRight, KeyCode::ShiftLeft] {
            assert!(keyboard.event(code, true, false), "{code:?} is still physically held");
            assert!(!keyboard.event(code, false, false));
            assert!(!keyboard.event(code, true, false), "a fresh {code:?} works after release");
        }
        assert!(!keyboard.event(KeyCode::Enter, true, false), "nonmovement actions are not latched");
    }

    #[test]
    fn focus_generated_keyboard_events_cannot_release_a_modal_hold_or_resume_walking() {
        let mut keyboard = GraphicsKeyboard::default();
        assert!(!keyboard.event(KeyCode::KeyW, true, false));
        keyboard.suppress_held();
        // Some platforms send this before Focused(false). It must not erase the hold.
        keyboard.synthetic_event(KeyCode::KeyW, false);
        assert!(keyboard.event(KeyCode::KeyW, true, true));
        keyboard.focus_lost();
        keyboard.synthetic_event(KeyCode::KeyW, true);
        assert!(keyboard.event(KeyCode::KeyW, true, true), "refocusing with W held cannot walk");
        assert!(!keyboard.event(KeyCode::KeyW, false, false));
        assert!(!keyboard.event(KeyCode::KeyW, true, false), "a physical release and press rearms W");

        keyboard.focus_lost();
        keyboard.synthetic_event(KeyCode::KeyW, false);
        // W was released while another window owned focus, so no release was delivered.
        // A genuinely fresh physical press after returning proves that intervening release.
        assert!(!keyboard.event(KeyCode::KeyW, true, false));
        keyboard.suppress_held();
        assert!(keyboard.event(KeyCode::KeyW, true, true), "a later modal still suppresses repeats");

        // A key first noticed by the platform on refocus is also held, not a new action.
        keyboard.synthetic_event(KeyCode::ArrowRight, true);
        assert!(keyboard.event(KeyCode::ArrowRight, true, true));
        assert!(!keyboard.event(KeyCode::ArrowRight, false, false));
        assert!(!keyboard.event(KeyCode::ArrowRight, true, false));
    }

    #[test]
    fn controller_modal_boundaries_precede_simultaneous_gameplay_or_setting_presses() {
        use crate::gamepad::{PadEvent, PadKey};
        let press = |key| PadEvent { key, pressed: true, repeat: false };
        let open = [press(PadKey::Confirm), press(PadKey::Invite), press(PadKey::Graphics)];
        assert_eq!(graphics_pad_priority(&open, false), Some(2), "L3 takes focus before A can talk or X can invite");
        assert_eq!(graphics_pad_priority(&open, true), Some(2), "L3 closes before A can change a setting");
        for close in [PadKey::Back, PadKey::Escape] {
            let events = [press(PadKey::Confirm), press(close)];
            assert_eq!(graphics_pad_priority(&events, true), Some(1));
            assert_eq!(graphics_pad_priority(&events, false), None, "ordinary controls keep their existing ordering");
        }
        for event in [PadEvent { key: PadKey::Graphics, pressed: false, repeat: false },
            PadEvent { key: PadKey::Graphics, pressed: true, repeat: true }] {
            assert_eq!(graphics_pad_priority(&[press(PadKey::Confirm), event], false), None);
        }
    }

    #[test]
    fn graphics_opens_only_during_idle_field_control() {
        let mut idle = state_with_peers(&[]);
        assert!(graphics_available(&idle, false, 0.0));
        assert!(!graphics_available(&idle, true, 0.0), "ordinary dialogue owns focus");
        idle.apply(0.0, ServerMsg::StoryPending {
            id: 1, area: AreaId { game: GameId::Fixture, key: "k".into() },
            pos: Pos::default(), facing: 0.0,
        });
        assert!(!graphics_available(&idle, false, 0.0), "a durable hold owns field control");
        idle.apply(0.0, ServerMsg::StoryCommitted { id: 1 });
        assert!(graphics_available(&idle, false, 0.0));
        let mut battle = state_in_a_preemptive_battle();
        assert!(!graphics_available(&battle, false, 0.0));
        battle.apply(10.0, ServerMsg::BattleEnd {
            battle_id: fmv_protocol::BattleId(1), outcome: fmv_protocol::battle::Outcome::Victory, rewards: vec![],
        });
        assert!(!graphics_available(&battle, false, 10.0), "the result panel still owns focus");
    }

    #[test]
    fn a_story_hold_seen_and_released_in_one_pump_still_clears_old_directions() {
        let mut input = Input::default();
        input.set(KeyCode::KeyW, true);
        input.set(KeyCode::ShiftLeft, true);
        let mut seen = 0;
        assert!(!observe_story_pending(&mut seen, 0, &mut input));
        assert_eq!(input.dir(), (0.0, 1.0));
        // The pending flag may already be false again; the new epoch still records the hold.
        assert!(observe_story_pending(&mut seen, 1, &mut input));
        assert_eq!(input, Input::default());
        assert!(!observe_story_pending(&mut seen, 1, &mut input), "a repeated message does not create another hold");
        input.set(KeyCode::KeyD, true);
        assert!(!observe_story_pending(&mut seen, 1, &mut input));
        assert_eq!(input.dir(), (1.0, 0.0), "new input after release remains usable");
        assert!(observe_story_pending(&mut seen, 2, &mut input));
        assert_eq!(input, Input::default());
    }

    #[test]
    fn field_key_repeats_cannot_restore_movement_retired_by_a_story_hold() {
        let mut input = Input::default();
        assert!(input.field_event(KeyCode::KeyW, true, false));
        assert!(input.field_event(KeyCode::ShiftLeft, true, false));
        let mut epoch = 0;
        assert!(observe_story_pending(&mut epoch, 1, &mut input));
        assert!(!input.field_event(KeyCode::KeyW, true, true));
        assert!(!input.field_event(KeyCode::ShiftLeft, true, true));
        assert_eq!(input, Input::default(), "a held keyboard key needs a new press after the save");
        assert!(input.field_event(KeyCode::KeyW, false, true), "releases always survive the repeat guard");
        assert!(input.field_event(KeyCode::KeyW, true, false));
        assert_eq!(input.dir(), (0.0, 1.0));
        assert_eq!(route_key(KeyCode::KeyW, true, true, true), KeyRoute::Battle(BattleKey::Up), "battle menu repeats never enter the field movement guard");
        assert_eq!(story_pending_route(KeyCode::KeyW, true, true, true), KeyRoute::Battle(BattleKey::Up));
    }

    fn info(fixture: bool) -> SessionInfo {
        SessionInfo {
            name: "Cloud".into(),
            tint: [1, 2, 3],
            area_id: AreaId {
                game: if fixture { GameId::Fixture } else { GameId::Ff7 },
                key: "k".into(),
            },
            fixture,
            card: fmv_protocol::CharacterCard::plain("Cloud", [1, 2, 3]),
        }
    }

    #[test]
    fn winit_os_errors_lose_their_source_location() {
        let raw = "os error at /x/winit-0.30.13/src/platform_impl/linux/mod.rs:765: neither WAYLAND_DISPLAY nor DISPLAY is set.";
        assert_eq!(friendly_os_error(raw), "neither WAYLAND_DISPLAY nor DISPLAY is set.");
        assert_eq!(
            friendly_os_error("something else: entirely"),
            "something else: entirely"
        );
        assert_eq!(friendly_os_error("os error at no-message"), "os error at no-message");
    }

    #[test]
    fn fixture_windows_are_always_labelled() {
        let t = base_title(&info(true));
        assert!(
            t.contains("FIXTURE") && t.contains("not FF7") && t.contains("Cloud"),
            "{t}"
        );
        let t = base_title(&info(false));
        assert!(!t.contains("FIXTURE"), "{t}");
    }

    #[test]
    fn a_real_ff7_field_is_titled_with_its_name_and_no_fixture_label() {
        let mut i = info(false);
        i.area_id.key = "md1stin".into();
        assert_eq!(base_title(&i), "FF Multiverse \u{2014} FF7 md1stin");
        // Another real game keeps the generic title (and never claims to be FF7).
        i.area_id.game = GameId::Ff8;
        let t = base_title(&i);
        assert!(!t.contains("FF7") && !t.contains("FIXTURE"), "{t}");
        // A fixture is a fixture whatever its id says.
        let mut f = info(true);
        f.area_id = AreaId { game: GameId::Ff7, key: "md1stin".into() };
        assert!(base_title(&f).contains("FIXTURE"), "{}", base_title(&f));
    }

    #[test]
    fn wasd_and_arrows_map_to_directions_and_opposites_cancel() {
        let mut i = Input::default();
        assert_eq!(i.dir(), (0.0, 0.0));
        assert!(i.set(KeyCode::KeyW, true));
        assert_eq!(i.dir(), (0.0, 1.0));
        assert!(i.set(KeyCode::ArrowRight, true));
        assert_eq!(i.dir(), (1.0, 1.0));
        assert!(i.set(KeyCode::KeyS, true));
        assert_eq!(i.dir(), (1.0, 0.0), "W and S cancel");
        assert!(i.set(KeyCode::KeyA, true));
        assert_eq!(i.dir(), (0.0, 0.0), "A and Right cancel");
        assert!(i.set(KeyCode::ArrowLeft, true) && i.set(KeyCode::KeyD, false) && i.set(KeyCode::ArrowDown, true));
        assert_eq!(i.dir(), (-1.0, 0.0));
        assert!(!i.set(KeyCode::Space, true), "other keys are ignored");
        assert!(!i.set(KeyCode::Escape, true));
        // Losing focus resets everything.
        i = Input::default();
        assert_eq!(i.dir(), (0.0, 0.0));
    }

    /// Shift (either one) means walk instead of run; it is not a direction, and losing focus (which
    /// resets the whole input) lets go of it.
    #[test]
    fn shift_is_tracked_as_walking_and_focus_loss_lets_go_of_it() {
        let mut i = Input::default();
        assert!(!i.walking(), "running is the default");
        assert!(i.set(KeyCode::ShiftLeft, true));
        assert!(i.walking());
        assert_eq!(i.dir(), (0.0, 0.0), "Shift alone is no movement");
        assert!(i.set(KeyCode::ShiftRight, true) && i.set(KeyCode::ShiftLeft, false));
        assert!(i.walking(), "the right Shift is still held");
        assert!(i.set(KeyCode::ShiftRight, false));
        assert!(!i.walking());
        // Shift and a direction are independent.
        i.set(KeyCode::KeyW, true);
        i.set(KeyCode::ShiftLeft, true);
        assert_eq!((i.dir(), i.walking()), ((0.0, 1.0), true));
        // What a lost focus does: back to the default, nothing held.
        i = Input::default();
        assert_eq!((i.dir(), i.walking()), ((0.0, 0.0), false));
    }

    /// In an FF7 field the local player runs at the field's run speed, walks at its walk speed
    /// with Shift, never faster than the server allows; anywhere else the old single pace.
    #[test]
    fn the_local_player_runs_by_default_and_walks_with_shift_within_the_servers_cap() {
        let stage = crate::avatar::frame::tests::stage(); // walk 60, run 240
        assert_eq!(movement_pace(Some(&stage), false, 600.0), (240.0, AnimState::Run));
        assert_eq!(movement_pace(Some(&stage), true, 600.0), (60.0, AnimState::Walk));
        // The server's cap wins over both paces.
        assert_eq!(movement_pace(Some(&stage), false, 100.0), (100.0, AnimState::Run));
        assert_eq!(movement_pace(Some(&stage), true, 40.0), (40.0, AnimState::Walk));
        // Walking is slower than running, and both are within the cap for the field's own numbers.
        let (run, _) = movement_pace(Some(&stage), false, 600.0);
        let (walk, _) = movement_pace(Some(&stage), true, 600.0);
        assert!(walk < run && run <= 600.0);
        // No stage (a fixture, or a field seen from above): 0.8 of the cap, announced as walking,
        // Shift or not: exactly what the window did before.
        assert_eq!(movement_pace(None, false, 500.0), (400.0, AnimState::Walk));
        assert_eq!(movement_pace(None, true, 500.0), (400.0, AnimState::Walk));
    }

    #[test]
    fn the_title_shows_players_and_rtt_and_only_the_party_things_that_are_true() {
        let base = "FF Multiverse - FIXTURE (synthetic, not FF7) - Cloud";
        assert_eq!(live_title(base, 3, "12 ms", 0, None), format!("{base} | players 3 | rtt 12 ms"));
        assert_eq!(live_title(base, 3, "-", 2, None), format!("{base} | players 3 | rtt - | party 2"));
        assert_eq!(
            live_title(base, 1, "-", 0, Some("Tifa")),
            format!("{base} | players 1 | rtt - | INVITE from Tifa: press Y")
        );
        assert_eq!(
            live_title(base, 4, "5 ms", 3, Some("Barret")),
            format!("{base} | players 4 | rtt 5 ms | party 3 | INVITE from Barret: press Y")
        );
        // The base title and the first suffix are exactly what they were before parties.
        assert!(live_title(base, 2, "1 ms", 0, None).starts_with(base));
    }

    #[test]
    fn party_keys_act_on_a_fresh_press_only() {
        for (code, key) in [(KeyCode::KeyP, PartyKey::Invite), (KeyCode::KeyY, PartyKey::Accept), (KeyCode::KeyL, PartyKey::Leave)] {
            assert_eq!(PartyKey::from_event(code, true, false), Some(key), "{code:?} pressed");
            assert_eq!(PartyKey::from_event(code, false, false), None, "{code:?} released");
            assert_eq!(PartyKey::from_event(code, true, true), None, "{code:?} auto-repeat of a held key");
            assert_eq!(PartyKey::from_event(code, false, true), None);
        }
        // Everything else is not a party key: movement, quitting, other letters.
        for code in [KeyCode::KeyW, KeyCode::KeyA, KeyCode::KeyS, KeyCode::KeyD, KeyCode::ArrowUp, KeyCode::Escape, KeyCode::Space, KeyCode::KeyN] {
            assert_eq!(PartyKey::from_event(code, true, false), None, "{code:?}");
        }
        // And a party key is not a movement key.
        let mut input = Input::default();
        assert!(!input.set(KeyCode::KeyP, true) && !input.set(KeyCode::KeyY, true) && !input.set(KeyCode::KeyL, true));
    }

    /// Escape never quits in a battle (a player looking for a way out of the battle screen closed
    /// the whole window that way), and in the field it takes a second press.
    #[test]
    fn escape_never_quits_in_a_battle_and_needs_a_second_press_in_the_field() {
        assert_eq!(escape_action(true, false), EscapeAction::IgnoreInBattle);
        assert_eq!(escape_action(true, true), EscapeAction::IgnoreInBattle, "even when armed before the battle began");
        assert_eq!(escape_action(false, false), EscapeAction::Arm);
        assert_eq!(escape_action(false, true), EscapeAction::Quit);
    }

    #[test]
    fn a_first_escape_only_counts_for_a_while() {
        let t = Instant::now();
        let until = Some(t + QUIT_CONFIRM);
        assert!(quit_armed(until, t));
        assert!(quit_armed(until, t + QUIT_CONFIRM - Duration::from_millis(1)));
        assert!(!quit_armed(until, t + QUIT_CONFIRM), "the second press came too late");
        assert!(!quit_armed(None, t), "nothing armed");
    }

    #[test]
    fn b_confirm_and_back_act_on_a_fresh_press_only_and_are_not_movement_or_party_keys() {
        for (code, key) in [
            (KeyCode::KeyB, BattleKey::Request),
            (KeyCode::Space, BattleKey::Confirm),
            (KeyCode::Enter, BattleKey::Confirm),
            (KeyCode::NumpadEnter, BattleKey::Confirm),
            (KeyCode::Backspace, BattleKey::Back),
        ] {
            for in_battle in [false, true] {
                assert_eq!(BattleKey::from_event(code, true, false, in_battle), Some(key), "{code:?} pressed (in a battle: {in_battle})");
                assert_eq!(BattleKey::from_event(code, false, false, in_battle), None, "{code:?} released");
                assert_eq!(BattleKey::from_event(code, true, true, in_battle), None, "{code:?} auto-repeat of a held key");
            }
            let mut input = Input::default();
            assert!(!input.set(code, true), "{code:?} is not a movement key");
            assert_eq!(PartyKey::from_event(code, true, false), None, "{code:?} is not a party key");
        }
        for code in [KeyCode::KeyP, KeyCode::KeyY, KeyCode::KeyL, KeyCode::Escape, KeyCode::F1, KeyCode::KeyN, KeyCode::ShiftLeft] {
            assert_eq!(BattleKey::from_event(code, true, false, true), None, "{code:?}");
        }
    }

    /// Tab and Y swap the menu to the next ready member, in a battle and on a fresh press only: outside one Y accepts an invite
    /// as ever and Tab is no key of the game, and a release or a repeat is never a swap.
    #[test]
    fn tab_and_y_swap_the_menu_in_a_battle_and_only_on_a_fresh_press() {
        for code in [KeyCode::Tab, KeyCode::KeyY] {
            assert_eq!(route_key(code, true, false, true), KeyRoute::Swap, "{code:?} pressed in a battle");
            assert_ne!(route_key(code, true, true, true), KeyRoute::Swap, "{code:?} held in a battle");
            assert_ne!(route_key(code, false, false, true), KeyRoute::Swap, "{code:?} released in a battle");
        }
        assert_eq!(route_key(KeyCode::KeyY, true, false, false), KeyRoute::Party(PartyKey::Accept), "in the field Y accepts an invite");
        assert_eq!(route_key(KeyCode::Tab, true, false, false), KeyRoute::Movement, "in the field Tab is no key of the game");
        // A release in a battle still reaches the held-key state, as every release does.
        assert_eq!(route_key(KeyCode::KeyY, false, false, true), KeyRoute::Movement);
    }

    #[test]
    fn the_directions_are_battle_keys_only_in_a_battle_and_they_repeat_there() {
        for (codes, key) in [
            ([KeyCode::ArrowUp, KeyCode::KeyW], BattleKey::Up),
            ([KeyCode::ArrowDown, KeyCode::KeyS], BattleKey::Down),
            ([KeyCode::ArrowLeft, KeyCode::KeyA], BattleKey::Left),
            ([KeyCode::ArrowRight, KeyCode::KeyD], BattleKey::Right),
        ] {
            for code in codes {
                assert_eq!(BattleKey::from_event(code, true, false, true), Some(key), "{code:?} in a battle");
                assert_eq!(BattleKey::from_event(code, true, true, true), Some(key), "{code:?} held in a battle scrolls a list");
                assert_eq!(BattleKey::from_event(code, false, false, true), None, "{code:?} released");
                assert_eq!(BattleKey::from_event(code, true, false, false), None, "{code:?} outside a battle is a movement key");
                assert_eq!(BattleKey::from_event(code, true, true, false), None);
            }
        }
    }

    /// Walking keeps working outside a battle, a battle takes the directions, and a key let go of in a
    /// battle still stops the walk it started in the field.
    #[test]
    fn the_field_keeps_its_arrows_and_wasd_and_a_release_always_reaches_the_walk() {
        let walk = [KeyCode::ArrowUp, KeyCode::KeyW, KeyCode::ArrowDown, KeyCode::KeyS, KeyCode::ArrowLeft, KeyCode::KeyA, KeyCode::ArrowRight, KeyCode::KeyD];
        for code in walk {
            for repeat in [false, true] {
                assert_eq!(route_key(code, true, repeat, false), KeyRoute::Movement, "{code:?} in the field walks");
            }
            assert!(matches!(route_key(code, true, false, true), KeyRoute::Battle(_)), "{code:?} in a battle drives the menu");
            for in_battle in [false, true] {
                assert_eq!(route_key(code, false, false, in_battle), KeyRoute::Movement, "{code:?} released (in a battle: {in_battle}) reaches the walk");
            }
        }
        // A walk started in the field, a battle begins, the key is let go: the walk stops.
        let mut input = Input::default();
        assert_eq!(route_key(KeyCode::KeyW, true, false, false), KeyRoute::Movement);
        input.set(KeyCode::KeyW, true);
        assert_eq!(input.dir(), (0.0, 1.0));
        assert_eq!(route_key(KeyCode::KeyW, false, false, true), KeyRoute::Movement);
        input.set(KeyCode::KeyW, false);
        assert_eq!(input.dir(), (0.0, 0.0));
        // The other keys keep their routes.
        assert_eq!(route_key(KeyCode::Escape, true, false, true), KeyRoute::Escape);
        assert_eq!(route_key(KeyCode::Escape, true, true, true), KeyRoute::Ignored, "a held Esc is not a second press");
        assert_eq!(route_key(KeyCode::Escape, false, false, true), KeyRoute::Ignored);
        assert_eq!(route_key(KeyCode::F1, true, false, false), KeyRoute::WalkMesh);
        assert_eq!(route_key(KeyCode::F2, true, false, false), KeyRoute::Camera);
        assert_eq!(route_key(KeyCode::F2, true, false, true), KeyRoute::Camera, "in a battle too (it changes nothing there)");
        assert_eq!(route_key(KeyCode::F2, true, true, false), KeyRoute::Ignored, "a held F2 does not flicker");
        assert_eq!(route_key(KeyCode::F2, false, false, false), KeyRoute::Ignored);
        assert_eq!(route_key(KeyCode::F3, true, false, false), KeyRoute::AntiAliasing);
        assert_eq!(route_key(KeyCode::F3, true, false, true), KeyRoute::AntiAliasing);
        assert_eq!(route_key(KeyCode::F3, true, true, false), KeyRoute::Ignored);
        assert_eq!(route_key(KeyCode::F3, false, false, false), KeyRoute::Ignored);
        for in_battle in [false, true] {
            assert_eq!(route_key(KeyCode::F4, true, false, in_battle), KeyRoute::RenderScale);
            assert_eq!(route_key(KeyCode::F4, true, true, in_battle), KeyRoute::Ignored, "holding F4 toggles only once");
            assert_eq!(route_key(KeyCode::F4, false, false, in_battle), KeyRoute::Ignored);
        }
        assert_eq!(route_key(KeyCode::KeyP, true, false, true), KeyRoute::Party(PartyKey::Invite));
        assert_eq!(route_key(KeyCode::Space, true, false, false), KeyRoute::Battle(BattleKey::Confirm));
        assert_eq!(route_key(KeyCode::ShiftLeft, true, false, true), KeyRoute::Movement, "Shift is still tracked");
    }

    /// A state in a battle that starts preemptive (our character's gauge is full at once), with
    /// synthetic rules standing in for the game's.
    fn state_in_a_preemptive_battle() -> ClientState {
        state_in_a_preemptive_battle_with_game_over(false)
    }

    fn state_in_a_preemptive_battle_with_game_over(game_over_on_defeat: bool) -> ClientState {
        use fmv_protocol::{BattleId, BattleMember, SyntheticRules};
        let area = AreaId { game: GameId::Fixture, key: "k".into() };
        let mut s = ClientState::new(EntityId(1), AccountId(1), Pos::default(), area);
        s.battle.set_rules(Some(Arc::new(SyntheticRules::default())));
        s.apply(
            0.0,
            ServerMsg::EncounterStart {
                battle_id: BattleId(1),
                formation_id: 3,
                seed: 5,
                party: vec![BattleMember::player(EntityId(1), AccountId(1), "P0", 0, 5, 0)],
                preemptive: true,
                back_attack: false,
                battle_speed: 0,
                ticks_per_second: 1000,
                escape_policy: fmv_protocol::EscapePolicy::Formation,
                victory_fanfare: true,
                game_over_on_defeat,
                initial_party: None,
                data_fingerprint: SyntheticRules::FINGERPRINT,
            },
        );
        s
    }

    #[test]
    fn acknowledged_authored_game_over_only_accepts_fresh_confirm_or_back_to_disconnect() {
        use fmv_protocol::BattleId;
        use fmv_protocol::battle::Outcome;
        let mut s = state_in_a_preemptive_battle_with_game_over(true);
        assert_eq!(game_over_route(&s, KeyCode::Enter, true, false), GameOverRoute::Inactive, "setup policy alone does not end a fight");
        s.apply(10.0, ServerMsg::BattleEnd { battle_id: BattleId(1), outcome: Outcome::Defeat, rewards: vec![] });
        for code in [KeyCode::Enter, KeyCode::NumpadEnter, KeyCode::Space, KeyCode::Escape, KeyCode::Backspace] {
            assert_eq!(game_over_route(&s, code, true, false), GameOverRoute::Disconnect);
            assert_eq!(game_over_route(&s, code, true, true), GameOverRoute::Held, "an old held key cannot disconnect automatically");
            assert_eq!(game_over_route(&s, code, false, false), GameOverRoute::Held);
        }
        for code in [KeyCode::KeyW, KeyCode::ArrowDown, KeyCode::KeyP, KeyCode::KeyB, KeyCode::F6] {
            assert_eq!(game_over_route(&s, code, true, false), GameOverRoute::Held, "no walking, party change, debug fight or scene preview");
        }
        assert!(!s.battle.lingering(1000.0));
        assert_eq!(game_over_route(&s, KeyCode::Enter, true, false), GameOverRoute::Disconnect, "the terminal state never becomes ordinary field input");
        assert!(s.field_controls_held());
        let mut debug = state_in_a_preemptive_battle();
        debug.apply(10.0, ServerMsg::BattleEnd { battle_id: BattleId(1), outcome: Outcome::Defeat, rewards: vec![] });
        assert_eq!(game_over_route(&debug, KeyCode::Enter, true, false), GameOverRoute::Inactive, "debug defeat keeps its existing result/recovery controls");
    }

    use crate::battle::Level;
    use fmv_protocol::battle::Command as EngineCommand;

    #[test]
    fn b_asks_for_a_battle_outside_one_only_and_the_menu_attacks_on_our_turn_only() {
        // Outside a battle: B asks, Enter has nothing to fight, the menu keys say nothing.
        let mut outside = state_with_peers(&[]);
        assert_eq!(battle_request(BattleKey::Request, &mut outside, 1.0), BattleRequest::Send(BattleCmd::RequestEncounter { formation: None }));
        assert_eq!(battle_request(BattleKey::Confirm, &mut outside, 1.0), BattleRequest::Nothing("no battle to fight in (press B to ask for one)"));
        for key in [BattleKey::Back, BattleKey::Up, BattleKey::Down, BattleKey::Left, BattleKey::Right] {
            assert_eq!(battle_request(key, &mut outside, 1.0), BattleRequest::Nothing(""), "{key:?}");
        }
        // In one: B has nothing to ask (the server would refuse). Enter opens the target selection,
        // a second Enter attacks the first enemy.
        let mut inside = state_in_a_preemptive_battle();
        assert!(inside.battle.in_battle());
        assert_eq!(battle_request(BattleKey::Request, &mut inside, 1.0), BattleRequest::Nothing("you are already in a battle"));
        assert_eq!(battle_request(BattleKey::Confirm, &mut inside, 1.0), BattleRequest::Menu, "Attack opens the target selection");
        match battle_request(BattleKey::Confirm, &mut inside, 1.0) {
            BattleRequest::Send(BattleCmd::Command { actor, command: EngineCommand::Attack { target }, .. }) => {
                assert_eq!((actor.0, target.0), (0, 1), "our character hits the first enemy, which follows the party");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(inside.battle.view(1.0).map(|v| v.enemies.len()), Some(1));
    }

    #[test]
    fn the_menu_moves_with_the_directions_and_greyed_out_commands_say_why() {
        let mut s = state_in_a_preemptive_battle();
        let level = |s: &ClientState| s.battle.view(1.0).and_then(|v| v.menu).map(|m| (m.level, m.command_cursor));
        assert_eq!(level(&s), Some((Level::Commands, 0)));
        assert_eq!(battle_request(BattleKey::Down, &mut s, 1.0), BattleRequest::Menu);
        assert_eq!(level(&s).map(|l| l.1), Some(1), "the cursor is on Magic");
        // The battle's own engine gave the menu its spells when the battle started: Magic opens its list.
        assert_eq!(battle_request(BattleKey::Confirm, &mut s, 1.0), BattleRequest::Menu);
        assert_eq!(level(&s).map(|l| l.0), Some(Level::Magic));
        assert_eq!(battle_request(BattleKey::Back, &mut s, 1.0), BattleRequest::Menu);
        assert_eq!(level(&s).map(|l| l.0), Some(Level::Commands));
        // No inventory has arrived (the server has told us of no items): Item is refused with its reason.
        assert_eq!(battle_request(BattleKey::Down, &mut s, 1.0), BattleRequest::Menu);
        assert_eq!(level(&s).map(|l| l.1), Some(2), "the cursor is on Item");
        assert_eq!(battle_request(BattleKey::Confirm, &mut s, 1.0), BattleRequest::Nothing("No items to use"));
        for _ in 0..2 {
            assert_eq!(battle_request(BattleKey::Up, &mut s, 1.0), BattleRequest::Menu);
        }
        assert_eq!(level(&s).map(|l| l.1), Some(0));
        // Back at the top level is nobody's: it falls through to the window.
        assert_eq!(battle_request(BattleKey::Back, &mut s, 1.0), BattleRequest::Nothing(""));
    }

    /// The inventory the server sends is what the Item list holds: before it Item is greyed out with its
    /// reason, after it the items the engine can use are listed, and a Potion aimed at us goes out as the
    /// engine's Item command.
    #[test]
    fn the_inventory_the_server_sends_fills_the_item_list() {
        use fmv_protocol::InventoryEntry;
        let mut s = state_in_a_preemptive_battle();
        for _ in 0..2 {
            assert_eq!(battle_request(BattleKey::Down, &mut s, 1.0), BattleRequest::Menu);
        }
        assert_eq!(battle_request(BattleKey::Confirm, &mut s, 1.0), BattleRequest::Nothing("No items to use"));
        assert_eq!(s.apply(1.1, ServerMsg::Inventory { items: vec![InventoryEntry::new(0, 3), InventoryEntry::new(99, 1)] }), None);
        assert_eq!(battle_request(BattleKey::Confirm, &mut s, 1.2), BattleRequest::Menu, "Item opens its list now");
        let list = s.battle.view(1.2).and_then(|v| v.menu).and_then(|m| m.list).expect("the item list");
        assert_eq!((list.title, list.total, list.rows[0].right.as_str()), ("Item", 1, "x3"), "the engine can use a Potion; item 99 is not listed");
        // Pick it and aim at ourselves (the only one in the party).
        assert_eq!(battle_request(BattleKey::Confirm, &mut s, 1.2), BattleRequest::Menu);
        match battle_request(BattleKey::Confirm, &mut s, 1.2) {
            BattleRequest::Send(BattleCmd::Command { actor, command: EngineCommand::Item { item, target }, .. }) => assert_eq!((actor.0, item, target), (0, 0, fmv_protocol::Target::One(fmv_protocol::ActorId(0)))),
            other => panic!("{other:?}"),
        }
    }

    /// Spells, items and Defend are in the menu once the model has them (the client fills it itself; this
    /// test puts its own lists in), and what they send is the engine's own command of the same ids.
    #[test]
    fn magic_items_and_defend_send_the_engine_commands() {
        use crate::battle::{MenuItem, MenuSpell, TargetRule};
        let mut s = state_in_a_preemptive_battle();
        s.battle.menu_model_mut().spells = vec![MenuSpell::new(1, "Fire", 0, TargetRule::Enemies)];
        s.battle.menu_model_mut().items = vec![MenuItem::new(2, "Potion", 1, TargetRule::Allies)];
        s.battle.menu_model_mut().defend = true;
        let sent = |s: &mut ClientState, key: BattleKey| match battle_request(key, s, 1.0) {
            BattleRequest::Send(BattleCmd::Command { actor, command, .. }) => (actor.0, command),
            other => panic!("{other:?}"),
        };
        for key in [BattleKey::Down, BattleKey::Confirm] {
            assert_eq!(battle_request(key, &mut s, 1.0), BattleRequest::Menu);
        }
        assert_eq!(s.battle.view(1.0).and_then(|v| v.menu).and_then(|m| m.list).map(|l| l.title), Some("Magic"));
        // Pick Fire, aim at the dummy (actor 1, after our own), confirm: the engine's Magic command.
        assert_eq!(battle_request(BattleKey::Confirm, &mut s, 1.0), BattleRequest::Menu);
        assert_eq!(sent(&mut s, BattleKey::Confirm), (0, EngineCommand::Magic { spell: 1, target: fmv_protocol::Target::One(fmv_protocol::ActorId(1)) }));
        assert_eq!(s.battle.view(1.0).and_then(|v| v.menu).map(|m| m.command_cursor), Some(1), "back at the top, on Magic");
        // A Potion: the item list, then the party to aim at (we are the only one), then the Item command.
        for key in [BattleKey::Down, BattleKey::Confirm, BattleKey::Confirm] {
            assert_eq!(battle_request(key, &mut s, 1.0), BattleRequest::Menu);
        }
        assert_eq!(sent(&mut s, BattleKey::Confirm), (0, EngineCommand::Item { item: 2, target: fmv_protocol::Target::One(fmv_protocol::ActorId(0)) }));
        // Defend sends at once when the model has it.
        assert_eq!(battle_request(BattleKey::Down, &mut s, 1.0), BattleRequest::Menu);
        assert_eq!(sent(&mut s, BattleKey::Confirm), (0, EngineCommand::Defend));
    }

    /// Esc backs out of the menu one level at a time, and at no level of a battle (nor after one, until
    /// the panel is gone) does it end in a quit, armed or not.
    #[test]
    fn escape_goes_back_through_the_menu_and_never_quits_in_a_battle() {
        use crate::battle::{MenuItem, MenuSpell, TargetRule};
        let never_quits = |s: &mut ClientState, armed: bool| {
            let route = escape_route(s, 1.0, armed);
            assert_ne!(route, EscapeRoute::Action(EscapeAction::Quit), "Esc quit in a battle (armed: {armed})");
            route
        };
        let mut s = state_in_a_preemptive_battle();
        s.battle.menu_model_mut().spells = vec![MenuSpell::new(1, "Fire", 0, TargetRule::Enemies)];
        s.battle.menu_model_mut().items = vec![MenuItem::new(2, "Potion", 1, TargetRule::Allies)];
        let at = |s: &ClientState| s.battle.view(1.0).and_then(|v| v.menu).map(|m| m.level);
        // Commands -> Magic -> Target and back out again, Esc at every step.
        battle_request(BattleKey::Down, &mut s, 1.0);
        battle_request(BattleKey::Confirm, &mut s, 1.0);
        battle_request(BattleKey::Confirm, &mut s, 1.0);
        assert_eq!(at(&s), Some(Level::Target));
        for want in [Level::Magic, Level::Commands] {
            assert_eq!(never_quits(&mut s, true), EscapeRoute::Battle, "Esc backs out of the menu");
            assert_eq!(at(&s), Some(want));
        }
        // At the top it is nobody's: the window's own action, which in a battle is to ignore it (and say so).
        for armed in [false, true] {
            assert_eq!(never_quits(&mut s, armed), EscapeRoute::Action(EscapeAction::IgnoreInBattle));
        }
        assert_eq!(at(&s), Some(Level::Commands), "and it does not close the menu");
        // In the field it is the window's: the first press arms quitting, a second one quits.
        let mut field = state_with_peers(&[]);
        assert_eq!(escape_route(&mut field, 1.0, false), EscapeRoute::Action(EscapeAction::Arm));
        assert_eq!(escape_route(&mut field, 1.0, true), EscapeRoute::Action(EscapeAction::Quit));
    }

    #[test]
    fn after_a_battle_its_panel_takes_confirm_and_back_and_esc_does_not_quit_until_it_is_gone() {
        use fmv_protocol::{BattleId, BattleReward};
        let mut s = state_in_a_preemptive_battle();
        s.apply(
            10.0,
            ServerMsg::BattleEnd {
                battle_id: BattleId(1),
                outcome: fmv_protocol::battle::Outcome::Victory,
                rewards: vec![BattleReward::new(AccountId(1), 5, 6, 7)],
            },
        );
        assert!(!s.battle.in_battle(), "the server freed the player");
        assert!(s.battle.lingering(10.1), "but the screen lingers on the end");
        let finale = s.battle.view(10.1).and_then(|v| v.finale).expect("the panel");
        assert_eq!(finale.reward.map(|r| (r.exp, r.gil, r.ap)), Some((5, 6, 7)));
        // Too early to close it: the keys are taken and do nothing else (Esc must not arm quitting).
        for key in [BattleKey::Confirm, BattleKey::Back] {
            assert_eq!(battle_request(key, &mut s, 10.2), BattleRequest::Menu, "{key:?}");
        }
        assert_eq!(escape_route(&mut s, 10.2, false), EscapeRoute::Battle);
        assert!(s.battle.lingering(10.3), "still there");
        // Later a key closes it, and then the field's own rules are back.
        assert_eq!(battle_request(BattleKey::Confirm, &mut s, 12.0), BattleRequest::Menu);
        assert!(!s.battle.lingering(12.0) && s.battle.view(12.0).is_none());
        assert_eq!(escape_route(&mut s, 12.1, false), EscapeRoute::Action(EscapeAction::Arm));
    }

    // ---- the controller and the device -------------------------------------------------------------

    /// The controller is not a second input path: each key it can press goes through `route_key`
    /// like a key of the keyboard and lands where the documented keyboard key does.
    #[test]
    fn a_controller_key_lands_where_its_keyboard_key_does() {
        use crate::gamepad::PadKey;
        // (the key, where it goes in the field, where it goes in a battle)
        let table = [
            (PadKey::Up, KeyRoute::Movement, KeyRoute::Battle(BattleKey::Up)),
            (PadKey::Down, KeyRoute::Movement, KeyRoute::Battle(BattleKey::Down)),
            (PadKey::Left, KeyRoute::Movement, KeyRoute::Battle(BattleKey::Left)),
            (PadKey::Right, KeyRoute::Movement, KeyRoute::Battle(BattleKey::Right)),
            (PadKey::Confirm, KeyRoute::Battle(BattleKey::Confirm), KeyRoute::Battle(BattleKey::Confirm)),
            (PadKey::Back, KeyRoute::Battle(BattleKey::Back), KeyRoute::Battle(BattleKey::Back)),
            (PadKey::Invite, KeyRoute::Party(PartyKey::Invite), KeyRoute::Party(PartyKey::Invite)),
            // Y accepts an invite in the field; in a battle it is FF7's Triangle, which swaps the menu to the next ready member.
            (PadKey::Accept, KeyRoute::Party(PartyKey::Accept), KeyRoute::Swap),
            (PadKey::Leave, KeyRoute::Party(PartyKey::Leave), KeyRoute::Party(PartyKey::Leave)),
            (PadKey::Battle, KeyRoute::Battle(BattleKey::Request), KeyRoute::Battle(BattleKey::Request)),
            (PadKey::Escape, KeyRoute::Escape, KeyRoute::Escape),
            (PadKey::Walk, KeyRoute::Movement, KeyRoute::Movement),
            (PadKey::Camera, KeyRoute::Camera, KeyRoute::Camera),
            (PadKey::Graphics, KeyRoute::Ignored, KeyRoute::Ignored), // Modal routing handles F5 before gameplay.
        ];
        assert_eq!(table.len(), PadKey::ALL.len(), "every key is in the table");
        for (key, field, battle) in table {
            assert_eq!(route_key(key.key_code(), true, false, false), field, "{key:?} in the field");
            assert_eq!(route_key(key.key_code(), true, false, true), battle, "{key:?} in a battle");
            // A release never reaches the party, battle or quit handling: at most the walk sees it.
            for in_battle in [false, true] {
                let route = route_key(key.key_code(), false, false, in_battle);
                assert!(matches!(route, KeyRoute::Movement | KeyRoute::Ignored), "{key:?} released: {route:?}");
            }
        }
        // The directions repeat in a battle (a held stick scrolls a list), and nothing else does:
        // the pad only ever repeats directions, and Esc never counts twice.
        for key in PadKey::DIRECTIONS {
            assert!(matches!(route_key(key.key_code(), true, true, true), KeyRoute::Battle(_)), "{key:?}");
        }
        assert_eq!(route_key(PadKey::Escape.key_code(), true, true, false), KeyRoute::Ignored);
    }

    /// The directions and the left trigger drive the same held-key state the arrows and Shift do,
    /// and the trigger and the keyboard's left Shift do not let go of each other.
    #[test]
    fn the_stick_directions_walk_and_the_left_trigger_walks_slowly() {
        use crate::gamepad::PadKey;
        let mut input = Input::default();
        for (key, want) in [(PadKey::Up, (0.0, 1.0)), (PadKey::Down, (0.0, -1.0)), (PadKey::Left, (-1.0, 0.0)), (PadKey::Right, (1.0, 0.0))] {
            assert!(input.set(key.key_code(), true), "{key:?}");
            assert_eq!(input.dir(), want, "{key:?}");
            assert!(input.set(key.key_code(), false));
            assert_eq!(input.dir(), (0.0, 0.0));
        }
        assert!(!input.walking(), "running is the default");
        assert!(input.set(PadKey::Walk.key_code(), true));
        assert!(input.walking());
        input.set(KeyCode::ShiftLeft, true);
        input.set(PadKey::Walk.key_code(), false);
        assert!(input.walking(), "the keyboard's Shift is still down");
        input.set(KeyCode::ShiftLeft, false);
        assert!(!input.walking());
        // None of the other pad keys is a movement key.
        for key in [PadKey::Confirm, PadKey::Back, PadKey::Invite, PadKey::Accept, PadKey::Leave, PadKey::Battle, PadKey::Escape] {
            assert!(!input.set(key.key_code(), true), "{key:?}");
        }
    }

    #[test]
    fn a_notice_speaks_the_words_of_the_device_that_pressed_the_key() {
        use EscapeAction::{Arm, IgnoreInBattle, Quit};
        // The keyboard's words are the ones the window always showed.
        assert_eq!(escape_notice(IgnoreInBattle, InputSource::Keyboard), Some("Esc does not quit in a battle: close the window to quit"));
        assert_eq!(escape_notice(Arm, InputSource::Keyboard), Some("Press Esc again to quit"));
        assert_eq!(escape_notice(IgnoreInBattle, InputSource::Pad), Some("Start does not quit in a battle"));
        assert_eq!(escape_notice(Arm, InputSource::Pad), Some("Press Start again to quit"));
        for source in [InputSource::Keyboard, InputSource::Pad] {
            assert_eq!(escape_notice(Quit, source), None, "quitting says nothing");
        }
    }

    /// Full screen is borderless, on the current monitor, without any decoration (so no frame or
    /// title bar of any kind can show); a desktop gets the ordinary decorated window it always had.
    #[test]
    fn the_window_is_borderless_full_screen_only_when_the_device_asks_for_it() {
        let windowed = window_attributes("t".to_string(), DeviceMode::DESKTOP);
        assert_eq!(windowed.fullscreen, None);
        assert!(windowed.decorations);
        assert_eq!(windowed.inner_size, Some(LogicalSize::new(1100.0, 680.0).into()));
        // The Steam keyboard alone is not a reason for full screen.
        assert_eq!(window_attributes("t".to_string(), DeviceMode { fullscreen: false, steam_keyboard: true }).fullscreen, None);
        let full = window_attributes("FF".to_string(), DeviceMode { fullscreen: true, steam_keyboard: false });
        assert_eq!(full.fullscreen, Some(Fullscreen::Borderless(None)));
        assert!(!full.decorations);
        assert_eq!(full.title, "FF");
    }

    fn state_with_peers(peers: &[(u64, f32)]) -> ClientState {
        let area = AreaId { game: GameId::Fixture, key: "k".into() };
        let mut s = ClientState::new(EntityId(1), AccountId(1), Pos::default(), area);
        let entities = peers
            .iter()
            .map(|(id, x)| EntitySnapshot {
                id: EntityId(*id),
                pos: Pos::new(*x, 0.0, 0.0),
                facing: 0.0,
                anim: AnimState::Idle,
                avatar: AvatarAppearance::new(format!("p{id}"), [1, 2, 3], None),
            })
            .collect();
        s.apply(0.0, ServerMsg::EntityUpdate { tick: 1, entities });
        s
    }

    fn party_event(s: &mut ClientState, seq: u64, event: PartyEvent) {
        let names = vec![
            PartyName { account: AccountId(1), name: "Cloud".into(), entity: Some(EntityId(1)) },
            PartyName { account: AccountId(2), name: "Tifa".into(), entity: Some(EntityId(2)) },
            PartyName { account: AccountId(3), name: "Barret".into(), entity: Some(EntityId(3)) },
        ];
        s.apply(0.0, ServerMsg::PartyEvent { seq, event, names });
    }

    #[test]
    fn p_invites_the_nearest_other_player_and_nothing_when_nobody_is_there() {
        assert_eq!(party_request(PartyKey::Invite, &state_with_peers(&[]), 1.0), None);
        // Entity 2 is 10 units away, entity 3 is 5: the nearest wins, whatever the ids say.
        let s = state_with_peers(&[(2, 10.0), (3, -5.0)]);
        assert_eq!(party_request(PartyKey::Invite, &s, 1.0), Some(PartyCommand::Invite(EntityId(3))));
    }

    #[test]
    fn y_accepts_the_newest_pending_invite_and_nothing_without_one() {
        let mut s = state_with_peers(&[(2, 10.0), (3, 20.0)]);
        assert_eq!(party_request(PartyKey::Accept, &s, 1.0), None);
        // An invite of OURS is not one to accept.
        party_event(&mut s, 1, PartyEvent::InviteSent { invite: InviteId(1), from: AccountId(1), to: AccountId(2), expires_at: 9 });
        assert_eq!(party_request(PartyKey::Accept, &s, 1.0), None);
        party_event(&mut s, 2, PartyEvent::InviteSent { invite: InviteId(2), from: AccountId(2), to: AccountId(1), expires_at: 9 });
        party_event(&mut s, 3, PartyEvent::InviteSent { invite: InviteId(3), from: AccountId(3), to: AccountId(1), expires_at: 9 });
        assert_eq!(party_request(PartyKey::Accept, &s, 1.0), Some(PartyCommand::Accept(InviteId(3))));
        // The title says who it is from.
        assert_eq!(s.party.newest_invite_to_me().unwrap().from_name, "Barret");
        party_event(&mut s, 4, PartyEvent::InviteDeclined { invite: InviteId(3), from: AccountId(3), to: AccountId(1) });
        assert_eq!(party_request(PartyKey::Accept, &s, 1.0), Some(PartyCommand::Accept(InviteId(2))));
    }

    #[test]
    fn l_always_asks_to_leave_and_p_skips_people_already_in_the_party() {
        let mut s = state_with_peers(&[(2, 10.0), (3, 50.0)]);
        assert_eq!(party_request(PartyKey::Leave, &s, 1.0), Some(PartyCommand::Leave));
        party_event(&mut s, 1, PartyEvent::PartyFormed {
            party: PartyId(1),
            invite: InviteId(1),
            leader: AccountId(1),
            members: vec![AccountId(1), AccountId(2)],
        });
        assert_eq!(s.party.party_size(), 2);
        // The closest player (entity 2) is already in the party: the next one is invited.
        assert_eq!(party_request(PartyKey::Invite, &s, 1.0), Some(PartyCommand::Invite(EntityId(3))));
        assert_eq!(party_request(PartyKey::Leave, &s, 1.0), Some(PartyCommand::Leave));
        // The one-line description of what a key did never panics, whatever the request.
        for cmd in [PartyCommand::Invite(EntityId(3)), PartyCommand::Accept(InviteId(2)), PartyCommand::Leave, PartyCommand::Sync] {
            assert!(!describe_request(cmd).contains('\n'));
        }
        assert!(describe_request(PartyCommand::Invite(EntityId(3))).contains("entity 3"));
    }

    // ---- the frame of the field: players, the area's characters, models and markers -------------

    /// A state in the fixture's plaza whose snapshot names the given non-player characters
    /// (name, model, standing clip, x) and no other player.
    fn state_with_npcs(npcs: &[(&str, &str, Option<&str>, f32)]) -> ClientState {
        let area = AreaId { game: GameId::Fixture, key: "plaza".into() };
        let mut s = ClientState::new(EntityId(1), AccountId(1), Pos::new(0.0, -40.0, 0.0), area.clone());
        let npcs = npcs
            .iter()
            .enumerate()
            .map(|(i, (name, model, idle, x))| fmv_protocol::NpcSnapshot {
                id: i as u32,
                name: (*name).into(),
                model: (*model).into(),
                idle_animation: idle.map(str::to_string),
                pos: Pos::new(*x, 0.0, 0.0),
                facing: 0.0,
            })
            .collect();
        assert!(s.apply(0.0, ServerMsg::AreaSnapshot { area, tick: 1, entities: vec![], npcs }).is_none());
        s
    }

    /// Without models (no `char.lgp`, a fixture area) the characters of the area are markers, drawn
    /// before the players so that every player is over them, in the colour `npc_appearance` gives.
    #[test]
    fn without_models_the_areas_characters_are_markers_under_the_players() {
        use crate::npc::npc_appearance;
        let state = state_with_npcs(&[("gu0", "agyb", Some("acfe"), -60.0), ("gu1", "aagb", None, 60.0)]);
        let adapter = fmv_fixture::FixtureAdapter::new();
        let view = FieldView::top_down(fmv_adapter_api::GameAdapter::load_area(&adapter, "plaza").unwrap().as_ref());
        let mut epoch = 0;
        let draw = field_draw(None, &mut epoch, &view, &state, [1, 2, 3], 1.0, 0.0);
        assert!(draw.built.is_none());
        assert_eq!(epoch, state.npc_epoch(), "the frame noted which list of characters it saw");
        assert_eq!(draw.markers.len(), 3, "two characters and the local player");
        let npcs = state.npc_views();
        for (marker, npc) in draw.markers.iter().zip(npcs) {
            assert_eq!(marker.tint, npc_appearance(npc).marker_tint);
            assert!(!marker.own && !marker.party, "a character is neither the local player nor a party member");
            let at = view.to_view(npc.pos).unwrap();
            assert_eq!((marker.x, marker.y), (at.x, at.y), "it stands where its data puts it");
        }
        let own = draw.markers.last().unwrap();
        assert!(own.own && own.tint == [1, 2, 3], "the local player comes last");
        // The names are over the markers (there is no model to put them over).
        let spot = view.to_view(npcs[0].pos).unwrap();
        assert_eq!(label_anchor(None, &view, draw.own_who, Subject::Npc(0), npcs[0].pos), Some(spot));
    }

    /// With models: the character whose model loads is a model (and not also a marker), the one
    /// whose data names no model stays a marker, the player is a model too; the models were loaded
    /// when the list of characters was first seen, and their names go over their heads.
    #[test]
    fn with_models_the_areas_characters_are_models_and_the_ones_without_stay_markers() {
        use crate::avatar::frame::tests as scene;
        use crate::avatar::library::tests::Counting;
        use crate::avatar::library::AvatarLibrary;
        let state = state_with_npcs(&[("gu0", "agyb", Some("acfe"), -60.0), ("ghost", "", None, 60.0)]);
        let mut view = scene::view();
        view.stage = Some(scene::stage());
        let loader = Counting::new(std::rc::Rc::default(), vec![]);
        let asked = std::rc::Rc::clone(&loader.asked_npc);
        let mut avatars = Avatars::new(AvatarLibrary::new(Box::new(loader)));
        let mut epoch = 0;
        let draw = field_draw(Some(&mut avatars), &mut epoch, &view, &state, [1, 2, 3], 1.0, 0.016);
        assert_eq!(*asked.borrow(), ["agyb/acfe"], "asked for once, by the time the first frame was built");
        let built = draw.built.as_ref().expect("a stage and avatars: models");
        assert_eq!(built.frame.models.len(), 2, "the character with a model and the local player");
        // The players list is [gu0, ghost, own]: only `ghost` (index 1) has no model.
        assert_eq!(built.markers, [1]);
        assert_eq!(draw.markers.len(), 1, "one marker: the character the data names no model for");
        assert!(!draw.markers[0].own);
        // A second frame in the same state loads nothing and keeps the epoch.
        let again = field_draw(Some(&mut avatars), &mut epoch, &view, &state, [1, 2, 3], 1.0, 0.016);
        assert_eq!(asked.borrow().len(), 1);
        assert_eq!(again.built.unwrap().frame.models.len(), 2);
        // The model's name goes over its head; the markers' over the marker.
        let npcs = state.npc_views();
        let feet = view.to_view(npcs[0].pos).unwrap();
        let over = label_anchor(Some(built), &view, draw.own_who, Subject::Npc(0), npcs[0].pos).unwrap();
        assert!(over.y > feet.y + 10.0, "{over:?} over {feet:?}");
        let marker_spot = view.to_view(npcs[1].pos).unwrap();
        assert_eq!(label_anchor(Some(built), &view, draw.own_who, Subject::Npc(1), npcs[1].pos), Some(marker_spot));
    }
}
