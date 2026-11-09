//! The window handler for a run without `--name`: the character menu first, then the game, in one
//! window with one renderer.
//!
//! * **Menu phase.** The window opens at once (no connection yet). Key events go through
//!   [`input::menu_input`] to the [`Menu`] state machine; the [`MenuAction`]s it returns are run
//!   here: `Play` and `Register` build a login plan and a network configuration
//!   ([`flow::prepare`]) and start a [`Connector`], so the window stays alive while the handshake
//!   runs; `Forget` deletes a saved login from this machine; `Quit` leaves. A refusal comes back
//!   as a message on the screen it started from.
//! * **Join.** When the handshake succeeds the shell shows a "Loading" frame (decoding an FF7
//!   background takes a moment), then builds the [`Session`] and the field view exactly as the
//!   command line does, and hands window, renderer and session to the game's [`App`], which
//!   takes over every event from then on.
//!
//! The decisions (what an action means, what a failure says) live in [`super::menu`] and
//! [`super::flow`] and are tested without a window; this file only wires them to winit.

use super::connect::{Connected, Connector, Outcome};
use super::draw::DrawList;
use super::flow::{self, Env, Saved};
use super::hud::area_label;
use super::input::{menu_input, pad_menu_input, MenuInput};
use super::looks::LookCache;
use super::menu::{Menu, MenuAction, MenuConfig, Notice};
use super::screens;
use crate::accounts::{loose_mode_warning, TokenStore};
use crate::cli::exit;
use crate::ff7::SharedFf7;
use crate::gamemode::{osk_transition, request_osk, DeviceMode};
use crate::gamepad::{PadEvent, PadMode, Pads};
use crate::login::LoginPlan;
use crate::net::clean;
use crate::render::{FrameResult, Renderer};
use crate::session::Session;
use crate::window::{self, App, LoopResult};
use fmv_adapter_api::GameAdapter;
use fmv_core::GameId;
use fmv_ff7::Ff7Adapter;
use fmv_fixture::FixtureAdapter;
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

/// Frame pacing of the menu (about 60 Hz).
const FRAME: Duration = Duration::from_millis(16);
/// How often the saved characters are read again while the menu is up: a connection attempt that
/// was abandoned may still save its token, and another process may register a character.
const RELIST_EVERY: Duration = Duration::from_secs(1);
/// How long leaving waits for `Leave` to be flushed.
const LEAVE_WAIT: Duration = Duration::from_secs(1);
/// Frames the "Loading" screen is shown before the (blocking) field load starts.
const LOADING_FRAMES: u32 = 2;
const MENU_TITLE: &str = "FF Multiverse - choose your character";

/// What the shell needs from the command line.
pub struct Launch {
    /// The server, as `host:port`.
    pub server: String,
    pub store: TokenStore,
    pub looks: LookCache,
    /// `client_build` for `Hello`.
    pub client_build: String,
    /// The game whose content this machine holds (it decides the bodies and slots on offer and the labels).
    pub game: GameId,
    /// `--tint`: the colour the create form starts with.
    pub tint: Option<[u8; 3]>,
    /// FF7 only: this machine's opened adapter, shared with the network thread and the window.
    pub ff7: Option<Arc<Ff7Adapter>>,
    /// What the device asks of the window (full screen) and of the text fields (Steam's keyboard):
    /// decided once in `main` from the flags and the environment.
    pub device: DeviceMode,
}

impl Launch {
    fn env(&self) -> Env<'_> {
        Env { server: &self.server, store: &self.store, looks: &self.looks, client_build: &self.client_build }
    }

    /// A fresh adapter for a connection attempt (the network thread owns it; the content behind
    /// it is shared, so this is cheap and never indexes anything again).
    fn make_adapter(&self) -> Box<dyn GameAdapter> {
        match &self.ff7 {
            Some(adapter) => Box::new(SharedFf7(Arc::clone(adapter))),
            None => Box::new(FixtureAdapter::new()),
        }
    }
}

/// What to do after running an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Next {
    Stay,
    Quit,
}

/// A finished handshake waiting for the "Loading" frames to be shown.
struct Pending {
    connected: Box<Connected>,
    plan: LoginPlan,
    frames: u32,
}

pub struct Shell {
    launch: Launch,
    menu: Menu,
    saved: Saved,
    /// Token-file problems already printed (each is reported once, not every second).
    reported: Vec<String>,
    listed_at: Instant,
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    connector: Option<Connector>,
    /// How the running attempt logs in (for the message if it fails).
    plan: Option<LoginPlan>,
    pending: Option<Pending>,
    /// The game, once joined: it owns the window and the renderer from then on.
    game: Option<App>,
    shift: bool,
    command: bool,
    last_tick: Instant,
    failure: Option<String>,
    /// The game controllers. Closed until the window exists; handed to the game with the window.
    pads: Pads,
    /// Was a text field in focus at the last check (see [`Shell::sync_keyboard`])?
    text_focus: bool,
    /// Has the window been shown (a frame was presented) or focused? Steam's keyboard is only asked
    /// for after that, so that it has a window of ours to type into.
    shown: bool,
}

impl Shell {
    pub fn new(launch: Launch) -> Self {
        let saved = flow::saved_characters(&launch.env());
        let menu = Menu::new(MenuConfig::new(launch.server.clone(), launch.game).with_tint(launch.tint), saved.entries.clone());
        let now = Instant::now();
        let mut shell = Self {
            launch,
            menu,
            saved: Saved::default(),
            reported: Vec::new(),
            listed_at: now,
            window: None,
            renderer: None,
            connector: None,
            plan: None,
            pending: None,
            game: None,
            shift: false,
            command: false,
            last_tick: now,
            failure: None,
            pads: Pads::closed(),
            text_focus: false,
            shown: false,
        };
        shell.report_problems(&saved.problems);
        shell.saved = saved;
        shell
    }

    /// Print each token-file problem once, and tell the player there is something to look at.
    fn report_problems(&mut self, problems: &[String]) {
        let fresh: Vec<&String> = problems.iter().filter(|p| !self.reported.contains(p)).collect();
        for problem in &fresh {
            eprintln!("warning: {problem}");
        }
        if !fresh.is_empty() {
            self.menu.set_notice(
                Notice::warning(format!("{} saved login file(s) could not be used.", problems.len()))
                    .with_hint("The terminal says which; fix or delete them."),
            );
        }
        self.reported.extend(fresh.into_iter().cloned());
    }

    /// Read the saved characters again and bring the menu up to date.
    fn relist(&mut self) {
        let saved = flow::saved_characters(&self.launch.env());
        self.report_problems(&saved.problems);
        self.listed_at = Instant::now();
        let entries = saved.entries.clone();
        self.saved = saved;
        self.menu.refresh_accounts(entries);
    }

    // ----- menu input and actions ---------------------------------------------------------------

    fn handle_input(&mut self, input: MenuInput) -> Next {
        let action = match input {
            MenuInput::Nav(press) => self.menu.key(press),
            MenuInput::Text(text) => self.menu.text(&text),
        };
        action.map_or(Next::Stay, |a| self.run_action(a))
    }

    /// A controller button: turned into the menu input a key press would have been (see
    /// [`pad_menu_input`]) and handled like it, so the menu has one way in.
    fn handle_pad(&mut self, event: PadEvent) -> Next {
        match pad_menu_input(event.key, event.pressed, event.repeat, self.menu.pad_screen()) {
            Some(input) => self.handle_input(input),
            None => Next::Stay,
        }
    }

    /// Read the controllers (never blocks) and handle what they did.
    fn poll_pads(&mut self, el: &ActiveEventLoop) {
        for event in self.pads.poll(Instant::now(), PadMode::Menu) {
            if self.handle_pad(event) == Next::Quit {
                el.exit();
            }
        }
    }

    /// Ask Steam for its on-screen keyboard when a text field gains the focus and let it go when
    /// the field loses it, but only on a device that has one to ask (see `crate::gamemode`) and only
    /// once the window has been shown. A fresh install opens in the name field, so the first check
    /// after the window is up is the field gaining focus.
    fn sync_keyboard(&mut self) {
        if !self.shown {
            return;
        }
        let focused = self.menu.text_input_focused();
        if let Some(request) = osk_transition(self.launch.device, self.text_focus, focused) {
            request_osk(request);
        }
        self.text_focus = focused;
    }

    fn run_action(&mut self, action: MenuAction) -> Next {
        match action {
            MenuAction::Quit => return Next::Quit,
            MenuAction::CancelConnect => {
                // Dropping the connector abandons the attempt: if it still joins, its helper
                // thread says goodbye. A registration still saves its token, which the next
                // listing shows.
                self.connector = None;
                self.plan = None;
                eprintln!("connecting: cancelled");
                self.relist();
            }
            MenuAction::Forget { index } => {
                if let Some(account) = self.saved.accounts.get(index).cloned() {
                    let name = clean(&account.name);
                    match self.launch.store.forget(&self.launch.server, &account) {
                        Ok(()) => {
                            self.launch.looks.forget(&self.launch.server, &account.name);
                            eprintln!("account: forgot the saved login of \"{name}\" on this computer (the server keeps the character)");
                            self.relist();
                            self.menu.set_notice(Notice::info(format!("Forgot the saved login of {name}.")));
                        }
                        Err(e) => {
                            eprintln!("error: cannot forget the saved login of \"{name}\": {e}");
                            self.relist();
                            self.menu.set_notice(Notice::error(format!("Could not forget {name}: {e}")));
                        }
                    }
                }
            }
            MenuAction::Play { .. } | MenuAction::Register { .. } => self.start_attempt(&action),
        }
        Next::Stay
    }

    /// Start connecting for a `Play` or `Register` action; a problem before anything is sent
    /// goes straight back to the menu as a message.
    fn start_attempt(&mut self, action: &MenuAction) {
        let attempt = match flow::prepare(&self.launch.env(), &self.saved.accounts, action) {
            Ok(attempt) => attempt,
            Err(notice) => {
                eprintln!("error: {}", notice.text);
                self.menu.connect_failed(notice);
                return;
            }
        };
        if let LoginPlan::Token(account) = &attempt.plan {
            if let Some(mode) = account.loose_mode {
                eprintln!("{}", loose_mode_warning(&account.path, mode));
            }
        }
        eprintln!("connecting to {} ({})...", self.launch.server, attempt.plan.describe());
        match Connector::start(attempt.cfg, self.launch.make_adapter()) {
            Ok(connector) => {
                self.connector = Some(connector);
                self.plan = Some(attempt.plan);
            }
            Err(e) => {
                eprintln!("error: {e}");
                self.menu.connect_failed(Notice::error(e));
            }
        }
    }

    /// A connection attempt ended.
    fn on_outcome(&mut self, outcome: Outcome) {
        let plan = self.plan.take();
        match outcome {
            Outcome::Joined(connected) => {
                let Some(plan) = plan else {
                    // An attempt nobody is waiting for (cannot happen: it was abandoned): leave.
                    let Connected { mut net, .. } = *connected;
                    net.leave_and_join(LEAVE_WAIT);
                    return;
                };
                let what = area_label(&connected.joined.area_id, connected.joined.fixture);
                self.menu.loading(what);
                self.pending = Some(Pending { connected, plan, frames: 0 });
            }
            Outcome::Failed(error) => {
                // The full text on the terminal as the command line prints it; a token that could
                // not be saved is part of it on purpose (see `NetError::TokenNotSaved`).
                eprintln!("error: {error}");
                // The attempt may have saved a token before it failed: list again FIRST, so the
                // menu can put the player on that character instead of the form.
                self.relist();
                let plan = plan.unwrap_or(LoginPlan::Register { name: String::new(), replace: false });
                self.menu.connect_failed(flow::failure_notice(&error, &plan));
            }
        }
    }

    /// Poll the running attempt (never blocks).
    fn poll_connector(&mut self) {
        if let Some(connector) = self.connector.as_mut() {
            if let Some(outcome) = connector.poll() {
                self.connector = None;
                self.on_outcome(outcome);
            }
        }
    }

    // ----- phases -------------------------------------------------------------------------------

    fn fail(&mut self, el: &ActiveEventLoop, msg: String) {
        if self.failure.is_none() {
            self.failure = Some(msg);
        }
        el.exit();
    }

    /// Joined: hand the window over to the game.
    fn enter_game(&mut self, el: &ActiveEventLoop, pending: Pending) {
        let (Some(window), Some(renderer)) = (self.window.take(), self.renderer.take()) else {
            let Connected { mut net, .. } = *pending.connected;
            net.leave_and_join(LEAVE_WAIT);
            return self.fail(el, "the window went away while joining".to_string());
        };
        let mut app = self.begin_game(pending);
        app.adopt_surface(window, renderer);
        app.adopt_pads(std::mem::take(&mut self.pads));
        self.game = Some(app);
    }

    /// Everything about joining that needs no window: say what was joined, remember what the
    /// server holds of this character, and build the session and the field view exactly as the
    /// command line does.
    fn begin_game(&mut self, pending: Pending) -> App {
        let Pending { connected, plan, .. } = pending;
        let Connected { net, joined } = *connected;
        crate::announce_joined(&joined);
        if let LoginPlan::Token(account) = &plan {
            eprintln!("account: logged in with the saved token for \"{}\"", clean(&account.name));
        }
        self.launch.looks.remember_card(&self.launch.server, plan.name(), &joined.character);
        let mut session = Session::new(net, joined);
        if let Some(adapter) = &self.launch.ff7 {
            // This machine's own FF7 data lets the session follow the server to other areas.
            session = session.with_content(adapter.clone());
        }
        let view = crate::field_view(self.launch.ff7.as_deref(), &session);
        App::new(session, view).with_ff7(self.launch.ff7.clone()).with_device(self.launch.device)
    }

    fn tick(&mut self, el: &ActiveEventLoop) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f32();
        self.last_tick = now;
        self.menu.tick(dt);
        self.poll_pads(el);
        self.poll_connector();
        self.sync_keyboard();
        if let Some(pending) = self.pending.as_mut() {
            pending.frames += 1;
            if pending.frames > LOADING_FRAMES {
                if let Some(pending) = self.pending.take() {
                    self.enter_game(el, pending);
                }
            }
        }
        if self.game.is_some() {
            return;
        }
        if !self.menu.is_connecting() && self.pending.is_none() && now.duration_since(self.listed_at) >= RELIST_EVERY {
            self.relist();
        }
        if let Some(window) = &self.window {
            window.request_redraw();
        }
        el.set_control_flow(ControlFlow::WaitUntil(now + FRAME));
    }

    fn redraw(&mut self, el: &ActiveEventLoop) {
        let Some(renderer) = self.renderer.as_mut() else { return };
        let (w, h) = renderer.size();
        let mut overlay = DrawList::new();
        screens::draw_menu(&self.menu, w, h, &mut overlay);
        let result = renderer.render_with(&screens::menu_scene(), Some(&overlay));
        let gpu_error = renderer.first_gpu_error();
        // The first frame on screen: from now on the window can be typed into.
        if result == FrameResult::Presented {
            self.shown = true;
        }
        match (result, gpu_error) {
            (FrameResult::Fatal(msg), _) => self.fail(el, msg),
            (_, Some(e)) => self.fail(el, format!("GPU error: {e}")),
            _ => {}
        }
    }

    /// The exit code once the event loop ended with `result`.
    fn finish(&mut self, result: LoopResult) -> u8 {
        if let Some(app) = self.game.as_mut() {
            return app.finish(result);
        }
        // Left while a join was waiting for its first frames: say goodbye.
        if let Some(pending) = self.pending.take() {
            let Connected { mut net, .. } = *pending.connected;
            net.leave_and_join(LEAVE_WAIT);
        }
        // A connection attempt still running is dropped: if it joins, its thread says goodbye.
        self.connector = None;
        match (self.failure.take(), window::describe_loop_failure(&result)) {
            (Some(msg), _) | (None, Some(msg)) => {
                eprintln!("error: {msg}");
                exit::FAILURE
            }
            (None, None) => exit::OK,
        }
    }
}

impl ApplicationHandler for Shell {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_some() || self.game.is_some() {
            return;
        }
        let window = match el.create_window(window::window_attributes(MENU_TITLE.to_string(), self.launch.device)) {
            Ok(w) => Arc::new(w),
            Err(e) => return self.fail(el, format!("cannot create the window: {e}")),
        };
        match Renderer::new(Arc::clone(&window), el.owned_display_handle()) {
            Ok(r) => {
                eprintln!("renderer: {}", r.describe());
                self.renderer = Some(r);
            }
            Err(e) => return self.fail(el, format!("cannot start the renderer: {e}")),
        }
        self.window = Some(window);
        // Controllers are only read once there is a window to give them to.
        self.pads = Pads::open();
        self.last_tick = Instant::now();
    }

    fn window_event(&mut self, el: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if let Some(app) = self.game.as_mut() {
            return app.window_event(el, id, event);
        }
        match event {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::Resized(size) => {
                if let Some(r) = self.renderer.as_mut() {
                    r.resize(size.width, size.height);
                }
            }
            WindowEvent::ModifiersChanged(mods) => {
                let state = mods.state();
                self.shift = state.shift_key();
                self.command = state.control_key() || state.alt_key() || state.super_key();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let pressed = event.state == ElementState::Pressed;
                let input = menu_input(&event.logical_key, event.text.as_deref(), pressed, event.repeat, self.shift, self.command);
                if let Some(input) = input {
                    if self.handle_input(input) == Next::Quit {
                        el.exit();
                    }
                }
            }
            WindowEvent::Focused(focused) => {
                // Controllers are only heard while the window has the focus (see `crate::gamepad`),
                // and a window with the focus is a window the keyboard can type into.
                self.pads.set_focused(focused);
                if focused {
                    self.shown = true;
                } else {
                    // Releases while unfocused are never delivered: forget held modifiers.
                    self.shift = false;
                    self.command = false;
                }
            }
            WindowEvent::RedrawRequested => self.redraw(el),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        if let Some(app) = self.game.as_mut() {
            return app.about_to_wait(el);
        }
        if self.window.is_none() {
            return;
        }
        self.tick(el);
        // `tick` may have handed the window to the game: it paces itself from here on.
        if let Some(app) = self.game.as_mut() {
            app.about_to_wait(el);
        }
    }

    fn exiting(&mut self, el: &ActiveEventLoop) {
        if let Some(app) = self.game.as_mut() {
            app.exiting(el);
        }
    }
}

/// Run the pre-join menu and then the game on `event_loop`; the process exit code.
pub fn run(event_loop: EventLoop<()>, launch: Launch) -> u8 {
    let mut shell = Shell::new(launch);
    // A panic inside the windowing or GPU stack must not end as a raw abort.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| event_loop.run_app(&mut shell)));
    shell.finish(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gamemode::{take_asked, Osk};
    use crate::gamepad::PadKey;
    use crate::ui::menu::{KeyPress, Look, NavKey, NoticeKind, Screen};
    use crate::ui::testkit::{serve_join, Reply};
    use fmv_protocol::{RejectCode, Secret};
    use std::net::TcpListener;

    /// A launch on an ordinary desktop: windowed, and nothing is ever asked of Steam.
    fn launch(dir: &std::path::Path, server: &str) -> Launch {
        Launch {
            server: server.to_string(),
            store: TokenStore::new(dir),
            looks: LookCache::new(dir),
            client_build: "test-build".to_string(),
            game: GameId::Ff7,
            tint: None,
            ff7: None,
            device: DeviceMode::DESKTOP,
        }
    }

    /// A launch inside a gamescope session: full screen, and Steam's keyboard for text fields.
    fn game_mode_launch(dir: &std::path::Path, server: &str) -> Launch {
        Launch { device: DeviceMode { fullscreen: true, steam_keyboard: true }, ..launch(dir, server) }
    }

    /// A press of a controller button, as the pad layer delivers it.
    fn pad_press(key: PadKey) -> PadEvent {
        PadEvent { key, pressed: true, repeat: false }
    }

    /// An address nothing listens on.
    fn closed_port() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        drop(listener);
        addr
    }

    fn nav(key: NavKey) -> MenuInput {
        MenuInput::Nav(KeyPress::new(key))
    }

    fn wait_until_idle(shell: &mut Shell) {
        let start = Instant::now();
        while shell.menu.is_connecting() {
            shell.poll_connector();
            assert!(start.elapsed() < Duration::from_secs(20), "the attempt never ended");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn the_menu_lists_what_is_saved_and_a_fresh_install_opens_on_the_create_form() {
        let dir = tempfile::tempdir().unwrap();
        let shell = Shell::new(launch(dir.path(), "127.0.0.1:47770"));
        assert!(matches!(shell.menu.screen(), Screen::Create(_)), "nothing saved yet");
        let store = TokenStore::new(dir.path());
        store.save_new("127.0.0.1:47770", "Cloud", &Secret::new("tok_cloud")).unwrap();
        store.save_new("127.0.0.1:47770", "Tifa", &Secret::new("tok_tifa")).unwrap();
        LookCache::new(dir.path()).store("127.0.0.1:47770", "Tifa", &Look::plain([200, 50, 50], Some("ff7/tifa"))).unwrap();
        let shell = Shell::new(launch(dir.path(), "127.0.0.1:47770"));
        assert!(matches!(shell.menu.screen(), Screen::Select(_)));
        let names: Vec<&str> = shell.menu.accounts().iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Cloud", "Tifa"]);
        assert_eq!(shell.menu.accounts()[1].look.as_ref().map(|l| l.tint), Some([200, 50, 50]));
        assert_eq!(shell.menu.accounts()[0].look, None);
        // Another server has none of them.
        let other = Shell::new(launch(dir.path(), "10.0.0.9:1"));
        assert!(other.menu.accounts().is_empty());
    }

    #[test]
    fn relisting_picks_up_a_login_saved_behind_the_menus_back() {
        let dir = tempfile::tempdir().unwrap();
        let mut shell = Shell::new(launch(dir.path(), "127.0.0.1:47770"));
        assert!(shell.menu.accounts().is_empty());
        // An abandoned registration saved its token a moment after the menu looked.
        TokenStore::new(dir.path()).save_new("127.0.0.1:47770", "Aerith", &Secret::new("tok")).unwrap();
        shell.relist();
        assert_eq!(shell.menu.accounts().len(), 1);
        assert_eq!(shell.saved.accounts.len(), 1, "the logins and the menu entries stay in step");
    }

    #[test]
    fn forgetting_deletes_the_saved_login_here_and_updates_the_list() {
        let dir = tempfile::tempdir().unwrap();
        let store = TokenStore::new(dir.path());
        let server = "127.0.0.1:47770";
        store.save_new(server, "Cloud", &Secret::new("tok_cloud")).unwrap();
        store.save_new(server, "Tifa", &Secret::new("tok_tifa")).unwrap();
        let looks = LookCache::new(dir.path());
        looks.store(server, "Cloud", &Look::plain([1, 2, 3], None)).unwrap();
        let mut shell = Shell::new(launch(dir.path(), server));
        // Del asks first; the default answer keeps the login.
        assert_eq!(shell.handle_input(nav(NavKey::Delete)), Next::Stay);
        assert_eq!(shell.handle_input(nav(NavKey::Enter)), Next::Stay);
        assert_eq!(shell.menu.accounts().len(), 2, "Keep is the default answer");
        assert!(store.find(server, Some("Cloud")).unwrap().is_some());
        // Del, move to Forget, Enter: gone from disk and from the list; the other one stays.
        shell.handle_input(nav(NavKey::Delete));
        shell.handle_input(nav(NavKey::Right));
        assert_eq!(shell.handle_input(nav(NavKey::Enter)), Next::Stay);
        assert!(store.find(server, Some("Cloud")).unwrap().is_none(), "the token file is gone");
        assert_eq!(looks.load(server, "Cloud"), None, "and so is the note about its look");
        assert_eq!(shell.menu.accounts().len(), 1);
        assert_eq!(shell.menu.accounts()[0].name, "Tifa");
        assert!(store.find(server, Some("Tifa")).unwrap().is_some(), "the other login is untouched");
        let notice = shell.menu.notice().expect("it says what happened");
        assert!(notice.text.contains("Forgot") && notice.text.contains("Cloud"), "{notice:?}");
        // The last one too: the menu falls back to the create form.
        shell.handle_input(nav(NavKey::Delete));
        assert_eq!(shell.handle_input(MenuInput::Text("y".into())), Next::Stay);
        assert!(shell.menu.accounts().is_empty());
        assert!(matches!(shell.menu.screen(), Screen::Create(_)));
        assert!(store.list(server).accounts.is_empty());
    }

    #[test]
    fn escape_on_the_list_quits() {
        let dir = tempfile::tempdir().unwrap();
        TokenStore::new(dir.path()).save_new("127.0.0.1:47770", "Cloud", &Secret::new("t")).unwrap();
        let mut shell = Shell::new(launch(dir.path(), "127.0.0.1:47770"));
        assert_eq!(shell.handle_input(nav(NavKey::Escape)), Next::Quit);
    }

    #[test]
    fn a_login_that_cannot_connect_comes_back_to_the_list_with_the_reason() {
        let dir = tempfile::tempdir().unwrap();
        let server = closed_port();
        TokenStore::new(dir.path()).save_new(&server, "Cloud", &Secret::new("tok_cloud")).unwrap();
        let mut shell = Shell::new(launch(dir.path(), &server));
        assert_eq!(shell.handle_input(nav(NavKey::Enter)), Next::Stay);
        assert!(shell.menu.is_connecting() && shell.connector.is_some());
        wait_until_idle(&mut shell);
        assert!(matches!(shell.menu.screen(), Screen::Select(_)), "{:?}", shell.menu.screen());
        let notice = shell.menu.notice().expect("a message");
        assert_eq!(notice.kind, NoticeKind::Error);
        assert!(notice.text.contains("cannot connect"), "{notice:?}");
        assert!(notice.hint.as_deref().is_some_and(|h| h.contains("try again") || h.contains("press Enter")), "{notice:?}");
        // The saved login is untouched, and trying again is one key away.
        assert_eq!(shell.saved.accounts.len(), 1);
        assert_eq!(shell.handle_input(nav(NavKey::Enter)), Next::Stay);
        assert!(shell.menu.is_connecting());
    }

    #[test]
    fn a_refused_registration_leaves_no_login_behind_and_keeps_the_form() {
        let dir = tempfile::tempdir().unwrap();
        let server = closed_port();
        let mut shell = Shell::new(launch(dir.path(), &server));
        assert!(matches!(shell.menu.screen(), Screen::Create(_)));
        shell.handle_input(MenuInput::Text("Cloud".into()));
        assert_eq!(shell.handle_input(nav(NavKey::Enter)), Next::Stay);
        assert!(shell.menu.is_connecting());
        wait_until_idle(&mut shell);
        let Screen::Create(form) = shell.menu.screen() else { panic!("{:?}", shell.menu.screen()) };
        assert_eq!(form.name.text(), "Cloud", "the form is intact for another try");
        assert!(shell.menu.notice().is_some_and(|n| n.text.contains("cannot connect")));
        assert!(TokenStore::new(dir.path()).list(&server).accounts.is_empty(), "no token, no character here");
    }

    #[test]
    fn abandoning_an_attempt_returns_to_the_screen_and_drops_the_connector() {
        let dir = tempfile::tempdir().unwrap();
        let server = closed_port();
        TokenStore::new(dir.path()).save_new(&server, "Cloud", &Secret::new("tok_cloud")).unwrap();
        let mut shell = Shell::new(launch(dir.path(), &server));
        shell.handle_input(nav(NavKey::Enter));
        assert!(shell.connector.is_some());
        assert_eq!(shell.handle_input(nav(NavKey::Escape)), Next::Stay);
        assert!(shell.connector.is_none() && shell.plan.is_none());
        assert!(matches!(shell.menu.screen(), Screen::Select(_)));
        assert!(!shell.menu.is_connecting());
    }

    #[test]
    fn a_token_file_that_cannot_be_used_is_reported_once_and_does_not_hide_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let store = TokenStore::new(dir.path());
        let server = "127.0.0.1:47770";
        store.save_new(server, "Cloud", &Secret::new("tok_cloud")).unwrap();
        let tifa = store.save_new(server, "Tifa", &Secret::new("tok_tifa")).unwrap();
        std::fs::write(&tifa, "tok_without_a_name_line\n").unwrap();
        let mut shell = Shell::new(launch(dir.path(), server));
        assert_eq!(shell.menu.accounts().len(), 1, "the usable one is listed");
        let notice = shell.menu.notice().expect("the player is told");
        assert_eq!(notice.kind, NoticeKind::Warning);
        assert_eq!(shell.reported.len(), 1);
        // Listing again does not report it again.
        shell.menu.set_notice(Notice::info("cleared"));
        shell.relist();
        assert_eq!(shell.reported.len(), 1);
        assert_eq!(shell.menu.notice().map(|n| n.text.as_str()), Some("cleared"));
    }

    // ---- the whole thing, over a real socket ---------------------------------------------------

    /// Poll until the join is waiting for its first frames; panics if the attempt failed instead.
    fn wait_for_join(shell: &mut Shell) {
        let start = Instant::now();
        while shell.pending.is_none() {
            shell.poll_connector();
            assert!(
                shell.menu.is_connecting() || shell.pending.is_some(),
                "the attempt failed: {:?}",
                shell.menu.notice().map(|n| (&n.text, &n.hint))
            );
            assert!(start.elapsed() < Duration::from_secs(20), "the attempt never ended");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn hello_of(seen: &crate::ui::testkit::Seen) -> (fmv_protocol::Auth, fmv_protocol::NewCharacter, String) {
        match &seen.hello {
            fmv_protocol::ClientMsg::Hello { auth, character, client_build, .. } => (auth.clone(), character.clone(), client_build.clone()),
            other => panic!("{other:?}"),
        }
    }

    /// Key presses of a new player to a joined game: the server receives exactly what was chosen (the colour,
    /// a body and a combat slot), the issued token is saved at once under the name, and what the server then
    /// says about the character (its card) is what the list remembers.
    #[test]
    fn registering_through_the_menu_reaches_the_game_and_the_server_gets_what_was_chosen() {
        use crate::cli::tint_from_name;
        use fmv_protocol::{Auth, CharacterCard, NewCharacter};
        let second_body = fmv_ff7::playable::PLACEHOLDER_BODIES[1].archetype;
        let dir = tempfile::tempdir().unwrap();
        let stored = CharacterCard {
            name: "Tifa".into(),
            tint: [9, 8, 7],
            body: Some(second_body.into()),
            binding: Some(fmv_ff7::playable::PLACEHOLDER_BODIES[1].model.appearance_ref.into()),
            slot: Some(2),
            migrated: false,
        };
        let (addr, server) = serve_join(Reply::Join { stored: Some(stored.clone()), token: Some("fmvt_issued_by_the_fake"), gate: None });
        let mut shell = Shell::new(launch(dir.path(), &addr));
        shell.handle_input(MenuInput::Text("  Tifa ".into()));
        shell.handle_input(nav(NavKey::Tab)); // the body
        shell.handle_input(nav(NavKey::Right)); // the second one
        for _ in 0..5 {
            shell.handle_input(nav(NavKey::Tab)); // palette, red, green, blue, the slot
        }
        shell.handle_input(nav(NavKey::Right)); // Tifa's
        assert_eq!(shell.handle_input(nav(NavKey::Enter)), Next::Stay);
        assert!(shell.menu.is_connecting());
        wait_for_join(&mut shell);
        assert!(matches!(shell.menu.screen(), Screen::Loading(what) if what == "FIXTURE plaza"), "{:?}", shell.menu.screen());

        // The token the server issued was saved the moment its Welcome arrived.
        let saved = TokenStore::new(dir.path()).find(&addr, Some("Tifa")).unwrap().expect("the token was saved");
        assert_eq!(saved.token.expose(), "fmvt_issued_by_the_fake");
        // Joining remembers what the SERVER holds of the character, not what was asked for.
        let pending = shell.pending.take().expect("a join is waiting for its first frames");
        let mut app = shell.begin_game(pending);
        assert_eq!(
            LookCache::new(dir.path()).load(&addr, "Tifa"),
            Some(Look { tint: [9, 8, 7], appearance: stored.binding.clone(), body: Some(second_body.into()), slot: Some(2) }),
            "the server's card wins"
        );
        // Quitting the game says goodbye.
        assert_eq!(app.finish(Ok(Ok(()))), exit::OK);
        let seen = server.join().unwrap();
        let (auth, character, build) = hello_of(&seen);
        assert_eq!(auth, Auth::Register { display_name: "Tifa".into() }, "the cleaned name");
        // No colour was chosen: the command line's default for the name.
        assert_eq!(character, NewCharacter { tint: tint_from_name("Tifa"), body: Some(second_body.into()), slot: Some(2) });
        assert_eq!(build, "test-build");
        assert!(seen.join.is_some(), "the area was joined");
        assert!(seen.left, "and left politely");
    }

    #[test]
    fn playing_a_saved_character_logs_in_with_its_token_and_remembers_its_card() {
        use crate::cli::tint_from_name;
        use fmv_protocol::{Auth, CharacterCard, NewCharacter};
        let dir = tempfile::tempdir().unwrap();
        // An account that predates the character creator: its canon look is its binding, its slot the one it fought as.
        let stored = CharacterCard { name: "Cloud".into(), tint: [1, 2, 3], body: None, binding: Some("ff7/cloud".into()), slot: Some(0), migrated: true };
        let (addr, server) = serve_join(Reply::Join { stored: Some(stored), token: None, gate: None });
        TokenStore::new(dir.path()).save_new(&addr, "Cloud", &Secret::new("tok_cloud")).unwrap();
        let mut shell = Shell::new(launch(dir.path(), &addr));
        assert_eq!(shell.menu.accounts()[0].look, None, "never seen: look unknown");
        assert_eq!(shell.handle_input(nav(NavKey::Enter)), Next::Stay);
        wait_for_join(&mut shell);
        let pending = shell.pending.take().unwrap();
        let mut app = shell.begin_game(pending);
        assert_eq!(
            LookCache::new(dir.path()).load(&addr, "Cloud"),
            Some(Look { tint: [1, 2, 3], appearance: Some("ff7/cloud".into()), body: None, slot: Some(0) })
        );
        // Next time the list shows it.
        shell.relist();
        assert_eq!(shell.menu.accounts()[0].look.as_ref().map(|l| l.tint), Some([1, 2, 3]));
        assert_eq!(app.finish(Ok(Ok(()))), exit::OK);
        let seen = server.join().unwrap();
        let (auth, character, _) = hello_of(&seen);
        assert_eq!(auth, Auth::Token(Secret::new("tok_cloud")), "the saved token, never a registration");
        assert_eq!(character, NewCharacter::with_tint(tint_from_name("Cloud")), "a login asks for nothing but a colour");
        assert!(seen.left);
    }

    #[test]
    fn a_refused_name_over_a_real_socket_is_shown_in_the_servers_words_and_nothing_is_saved() {
        let dir = tempfile::tempdir().unwrap();
        let (addr, server) = serve_join(Reply::Reject(RejectCode::NameTaken, "that display name is already taken"));
        let mut shell = Shell::new(launch(dir.path(), &addr));
        shell.handle_input(MenuInput::Text("Cloud".into()));
        shell.handle_input(nav(NavKey::Enter));
        wait_until_idle(&mut shell);
        let Screen::Create(form) = shell.menu.screen() else { panic!("{:?}", shell.menu.screen()) };
        assert_eq!(form.name.text(), "Cloud", "the form is ready for another try");
        let notice = shell.menu.notice().expect("a message");
        assert!(notice.text.contains("that display name is already taken"), "{notice:?}");
        assert_eq!(notice.hint.as_deref(), Some("Change the name and press Enter again."));
        assert!(TokenStore::new(dir.path()).list(&addr).accounts.is_empty(), "no character exists here");
        let seen = server.join().unwrap();
        assert!(!seen.left && seen.join.is_none());
        // The screen says it too.
        let mut list = DrawList::new();
        screens::draw_menu(&shell.menu, 1100, 680, &mut list);
        // (The message wraps over lines: read the drawn text as one.)
        let said: String = list.texts().iter().map(|t| t.text.as_str()).collect::<Vec<_>>().join(" ");
        assert!(said.contains("that display name is") && said.contains("already taken"), "{said}");
        assert!(said.contains("Change the name"), "{said}");
    }

    /// A registration that succeeded on the server but whose token could not be written: the
    /// token is on screen (and on the terminal), never lost, and nothing else competes with it.
    #[test]
    fn a_token_that_cannot_be_saved_is_shown_on_screen_in_full() {
        let dir = tempfile::tempdir().unwrap();
        // The server holds its Welcome back until the test has taken both file names.
        let (release, gate) = std::sync::mpsc::channel();
        let (addr, server) = serve_join(Reply::Join { stored: None, token: Some("fmvt_the_only_copy_1234567890"), gate: Some(gate) });
        let mut shell = Shell::new(launch(dir.path(), &addr));
        shell.handle_input(MenuInput::Text("Tifa".into()));
        assert_eq!(shell.handle_input(nav(NavKey::Enter)), Next::Stay);
        assert!(shell.menu.is_connecting(), "the pre-flight check passed: a token could be kept");
        // While the server hesitates, both file names this account could use are taken by others.
        let store = TokenStore::new(dir.path());
        let accounts = dir.path().join(crate::accounts::ACCOUNTS_DIR);
        let key = crate::accounts::server_key(&addr);
        let other = |name: &str| crate::accounts::format_token_file(&Secret::new("tok_other"), name).unwrap();
        std::fs::write(accounts.join(format!("{key}.token")), other("Somebody")).unwrap();
        std::fs::write(accounts.join(format!("{key}.tifa.token")), other("Tifa2")).unwrap();
        release.send(()).unwrap();
        wait_until_idle(&mut shell);
        let notice = shell.menu.notice().expect("a message");
        assert!(notice.is_alert(), "{notice:?}");
        assert_eq!(notice.secret.as_ref().map(Secret::expose), Some("fmvt_the_only_copy_1234567890"));
        assert!(!format!("{:?}", shell.menu).contains("fmvt_the_only_copy"), "the menu's Debug never prints it");
        // The screen shows the whole token, on its own, until Enter.
        let mut list = DrawList::new();
        screens::draw_menu(&shell.menu, 1100, 680, &mut list);
        let on_screen: String = list.texts().iter().filter(|t| t.color == [255, 255, 255, 255]).map(|t| t.text.as_str()).collect();
        assert_eq!(on_screen, "fmvt_the_only_copy_1234567890");
        assert_eq!(shell.handle_input(nav(NavKey::Down)), Next::Stay);
        assert!(shell.menu.notice().is_some_and(|n| n.is_alert()), "other keys do not dismiss it");
        shell.handle_input(nav(NavKey::Enter));
        assert!(shell.menu.notice().is_none());
        // The other accounts' files are untouched and the new character has no login.
        assert!(store.find(&addr, Some("Tifa")).unwrap().is_none());
        drop(server);
    }

    // ---- the controller and the keyboard request --------------------------------------------------

    /// A controller alone plays the menu through the shell: the stick chooses, A connects (and a
    /// failure comes back with its reason), B cancels, X opens the form, Start leaves.
    #[test]
    fn a_controller_alone_plays_the_menu_through_the_shell() {
        let dir = tempfile::tempdir().unwrap();
        let server = closed_port();
        let store = TokenStore::new(dir.path());
        store.save_new(&server, "Cloud", &Secret::new("tok_cloud")).unwrap();
        store.save_new(&server, "Tifa", &Secret::new("tok_tifa")).unwrap();
        let mut shell = Shell::new(launch(dir.path(), &server));
        assert_eq!(shell.handle_pad(pad_press(PadKey::Down)), Next::Stay);
        assert_eq!(shell.menu.selected_name(), Some("Tifa"));
        // A starts the attempt; with nothing listening it fails and the list comes back with the reason.
        assert_eq!(shell.handle_pad(pad_press(PadKey::Confirm)), Next::Stay);
        assert!(shell.menu.is_connecting() && shell.connector.is_some());
        wait_until_idle(&mut shell);
        assert!(matches!(shell.menu.screen(), Screen::Select(_)));
        assert!(shell.menu.notice().is_some_and(|n| n.text.contains("cannot connect")));
        // B cancels a running attempt and drops the connector.
        shell.handle_pad(pad_press(PadKey::Confirm));
        assert!(shell.connector.is_some());
        assert_eq!(shell.handle_pad(pad_press(PadKey::Back)), Next::Stay);
        assert!(shell.connector.is_none() && matches!(shell.menu.screen(), Screen::Select(_)));
        // B on the list does nothing (it never quits); Start does.
        assert_eq!(shell.handle_pad(pad_press(PadKey::Back)), Next::Stay);
        assert_eq!(shell.handle_pad(PadEvent { key: PadKey::Escape, pressed: false, repeat: false }), Next::Stay, "a release is nothing");
        assert_eq!(shell.handle_pad(pad_press(PadKey::Escape)), Next::Quit);
        // X opens the create form.
        assert_eq!(shell.handle_pad(pad_press(PadKey::Invite)), Next::Stay);
        assert!(matches!(shell.menu.screen(), Screen::Create(_)));
        // The party keys that mean nothing here change nothing.
        for key in [PadKey::Leave, PadKey::Battle, PadKey::Walk, PadKey::Accept] {
            assert_eq!(shell.handle_pad(pad_press(key)), Next::Stay, "{key:?}");
        }
        assert!(matches!(shell.menu.screen(), Screen::Create(_)));
    }

    /// On a gamescope device the first check after the window is shown, in the name field, asks for
    /// Steam's keyboard; leaving the field (or registering) lets it go; coming back asks again.
    #[test]
    fn steams_keyboard_is_asked_for_when_the_name_field_gains_focus_and_dropped_when_it_loses_it() {
        let dir = tempfile::tempdir().unwrap();
        let server = closed_port();
        let _ = take_asked();
        let mut shell = Shell::new(game_mode_launch(dir.path(), &server));
        assert!(shell.menu.text_input_focused(), "a fresh install opens in the name field");
        // Before the window has been shown nothing is asked (Steam would have no window to type into).
        shell.sync_keyboard();
        assert_eq!(take_asked(), []);
        shell.shown = true;
        shell.sync_keyboard();
        assert_eq!(take_asked(), [Osk::Open]);
        shell.sync_keyboard();
        assert_eq!(take_asked(), [], "staying in the field asks for nothing more");
        // Down to the Body row: out of the field.
        shell.handle_pad(pad_press(PadKey::Down));
        shell.sync_keyboard();
        assert_eq!(take_asked(), [Osk::Close]);
        // Back up: the field is entered again.
        shell.handle_pad(pad_press(PadKey::Up));
        shell.sync_keyboard();
        assert_eq!(take_asked(), [Osk::Open]);
        // Registering (a name, then A) leaves the field; the refusal puts the player back in it.
        shell.handle_input(MenuInput::Text("Cloud".into()));
        shell.handle_pad(pad_press(PadKey::Confirm));
        shell.sync_keyboard();
        assert_eq!(take_asked(), [Osk::Close], "the attempt runs: the keyboard goes away");
        wait_until_idle(&mut shell);
        shell.sync_keyboard();
        assert_eq!(take_asked(), [Osk::Open], "back in the name field after the refusal");
    }

    /// On the desktop nothing is ever asked of Steam, whatever the focus does.
    #[test]
    fn on_the_desktop_no_text_field_ever_asks_for_a_keyboard() {
        let dir = tempfile::tempdir().unwrap();
        let _ = take_asked();
        let mut shell = Shell::new(launch(dir.path(), "127.0.0.1:47770"));
        shell.shown = true;
        for key in [None, Some(PadKey::Down), Some(PadKey::Up), Some(PadKey::Down), Some(PadKey::Up)] {
            if let Some(key) = key {
                shell.handle_pad(pad_press(key));
            }
            shell.sync_keyboard();
        }
        assert!(shell.text_focus, "the focus was tracked all along");
        assert_eq!(take_asked(), [], "but Steam was never asked");
        // The same on a full-screen desktop (--fullscreen): full screen is not a reason for it.
        let mut shell = Shell::new(Launch { device: DeviceMode { fullscreen: true, steam_keyboard: false }, ..launch(dir.path(), "127.0.0.1:47770") });
        shell.shown = true;
        shell.sync_keyboard();
        assert_eq!(take_asked(), []);
    }

    #[test]
    fn the_adapter_of_an_attempt_matches_the_content_this_machine_holds() {
        let dir = tempfile::tempdir().unwrap();
        let l = launch(dir.path(), "127.0.0.1:47770");
        // Without FF7 content the synthetic fixture is what the handshake compares against.
        assert_eq!(l.make_adapter().game(), GameId::Fixture);
        assert_eq!(l.make_adapter().game(), GameId::Fixture, "and it can be made again for a retry");
    }
}
