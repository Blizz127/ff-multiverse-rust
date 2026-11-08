//! Window mode: winit 0.30 `ApplicationHandler` + the wgpu renderer.
//!
//! The event loop is created before the session starts (see `main`), so a missing display is
//! reported cleanly before any network traffic. Every start-up failure (window, GPU) becomes an
//! error message and a non-zero exit code, never a panic.

use crate::backdrop::FieldView;
use crate::cli::exit;
use crate::interp::render_time;
use crate::net::PartyCommand;
use crate::render::{FrameResult, PlayerSprite, Renderer, Scene};
use crate::session::{Session, SessionInfo};
use crate::state::ClientState;
use crate::ui::draw::DrawList;
use crate::ui::hud;
use fmv_core::GameId;
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

/// Walking speed as a fraction of the area's `max_speed` (the server enforces the maximum).
const WALK_SPEED_FACTOR: f32 = 0.8;
/// Frame pacing: wake up at least this often (about 60 Hz).
const FRAME: Duration = Duration::from_millis(16);
/// Longest simulated step, so a stalled frame cannot make the player jump.
const MAX_DT: f32 = 0.1;
const TITLE_REFRESH: Duration = Duration::from_secs(1);
/// How long quitting waits for `Leave` to be flushed.
const LEAVE_WAIT: Duration = Duration::from_secs(1);

pub fn create_event_loop() -> Result<EventLoop<()>, String> {
    // winit panics instead of returning an error when a system library it needs is missing
    // (for example libxkbcommon-x11 on X11). That is an environment problem, not a bug: turn
    // it into an ordinary error and keep the default panic output out of the user's way.
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let created = std::panic::catch_unwind(EventLoop::<()>::new);
    std::panic::set_hook(previous_hook);
    match created {
        Ok(Ok(event_loop)) => Ok(event_loop),
        Ok(Err(e)) => Err(friendly_os_error(&e.to_string())),
        Err(payload) => Err(format!(
            "the windowing system could not start ({}); is a required system library installed? \
             On X11 winit needs libxkbcommon-x11 and libxcb-xkb",
            panic_text(payload.as_ref())
        )),
    }
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
pub fn window_attributes(title: String) -> winit::window::WindowAttributes {
    Window::default_attributes()
        .with_title(title)
        .with_inner_size(LogicalSize::new(1100.0, 680.0))
        .with_min_inner_size(LogicalSize::new(320.0, 240.0))
}

pub fn run(event_loop: EventLoop<()>, session: Session, view: FieldView) -> u8 {
    let mut app = App::new(session, view);
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

/// Held movement keys.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Input {
    up: bool,
    down: bool,
    left: bool,
    right: bool,
}

impl Input {
    /// Record a key change; returns false for keys that are not movement keys.
    pub fn set(&mut self, code: KeyCode, pressed: bool) -> bool {
        match code {
            KeyCode::KeyW | KeyCode::ArrowUp => self.up = pressed,
            KeyCode::KeyS | KeyCode::ArrowDown => self.down = pressed,
            KeyCode::KeyA | KeyCode::ArrowLeft => self.left = pressed,
            KeyCode::KeyD | KeyCode::ArrowRight => self.right = pressed,
            _ => return false,
        }
        true
    }

    /// Movement direction in area axes (+x right, +y up); opposite keys cancel.
    pub fn dir(&self) -> (f32, f32) {
        let axis = |pos: bool, neg: bool| f32::from(i8::from(pos) - i8::from(neg));
        (axis(self.right, self.left), axis(self.up, self.down))
    }
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

pub(crate) struct App {
    session: Session,
    /// How the area is drawn: walkmesh and bounds in view space, the mapping, the picture.
    view: FieldView,
    base_title: String,
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    input: Input,
    last_tick: Instant,
    last_title: Instant,
    /// What the title last said about the party: its size and the pending invite. A change shows
    /// at once instead of waiting for the next refresh.
    last_title_party: (usize, Option<fmv_protocol::InviteId>),
    left: bool,
    failure: Option<String>,
}

impl App {
    pub(crate) fn new(session: Session, view: FieldView) -> Self {
        let base_title = base_title(&session.info);
        let now = Instant::now();
        Self {
            session,
            view,
            base_title,
            window: None,
            renderer: None,
            input: Input::default(),
            last_tick: now,
            last_title: now.checked_sub(TITLE_REFRESH).unwrap_or(now),
            last_title_party: (0, None),
            left: false,
            failure: None,
        }
    }

    /// Take over a window and renderer that already exist (the character menu opens them first
    /// and hands them over when the player joins). The window gets the game's title on the next
    /// tick.
    pub(crate) fn adopt_surface(&mut self, window: Arc<Window>, renderer: Renderer) {
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
        if self.last_title.elapsed() < TITLE_REFRESH && party_key == self.last_title_party {
            return;
        }
        self.last_title = Instant::now();
        self.last_title_party = party_key;
        let rtt = self
            .session
            .state
            .last_rtt()
            .map_or_else(|| "-".to_string(), |d| format!("{} ms", d.as_millis()));
        window.set_title(&live_title(
            &self.base_title,
            self.session.state.remote_count() + 1,
            &rtt,
            party_key.0,
            invite.as_ref().map(|i| i.from_name.as_str()),
        ));
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

    fn tick(&mut self, el: &ActiveEventLoop) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f32().min(MAX_DT);
        self.last_tick = now;
        if let Some(reason) = self.session.pump() {
            self.fail(el, reason);
            return;
        }
        self.session.log_party_notes();
        self.session.flush_party_sync();
        let speed = WALK_SPEED_FACTOR * self.session.area.max_speed();
        // The keys mean directions on the SCREEN; the area is walked in its own axes.
        let keys = self.input.dir();
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
            .walk(self.session.area.as_ref(), dir, speed, dt);
        self.session.flush_intent();
        self.update_title();
    }

    fn redraw(&mut self, el: &ActiveEventLoop) {
        let Some(renderer) = self.renderer.as_mut() else { return };
        let now = self.session.now();
        // Players are placed through the view; one that cannot be drawn (behind the camera) is
        // simply not shown this frame.
        let view = &self.view;
        let mut players: Vec<PlayerSprite> = self
            .session
            .state
            .remote_views(render_time(now))
            .iter()
            .filter_map(|v| {
                view.to_view(v.pose.pos).map(|p| PlayerSprite {
                    x: p.x,
                    y: p.y,
                    tint: v.tint,
                    own: false,
                    party: self.session.state.party.is_party_entity(v.id),
                })
            })
            .collect();
        if let Some(p) = view.to_view(self.session.state.own.pos()) {
            players.push(PlayerSprite {
                x: p.x,
                y: p.y,
                tint: self.session.info.tint,
                own: true,
                party: self.session.state.party.party_size() > 0,
            });
        }
        let scene = Scene {
            bounds: view.bounds,
            triangles: &view.triangles,
            players: &players,
            fixture_banner: self.session.info.fixture,
            backdrop: view.backdrop.as_ref(),
        };
        // Names, party, area, connection (and the FIXTURE label) over the scene.
        let (vw, vh) = renderer.size();
        let mut overlay = DrawList::new();
        hud::draw_hud(&hud::gather(&self.session, view), vw, vh, &mut overlay);
        let result = renderer.render_with(&scene, Some(&overlay));
        let gpu_error = renderer.first_gpu_error();
        match (result, gpu_error) {
            (FrameResult::Fatal(msg), _) => self.fail(el, msg),
            (_, Some(e)) => self.fail(el, format!("GPU error: {e}")),
            _ => {}
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = window_attributes(self.base_title.clone());
        let window = match el.create_window(attrs) {
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
                ..
            } => {
                let pressed = state == ElementState::Pressed;
                if code == KeyCode::Escape {
                    if pressed {
                        self.quit(el);
                    }
                } else if let Some(key) = PartyKey::from_event(code, pressed, repeat) {
                    self.party_key(key);
                } else {
                    self.input.set(code, pressed);
                }
            }
            // Releases while unfocused are never delivered: do not keep walking.
            WindowEvent::Focused(false) => self.input = Input::default(),
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

    fn info(fixture: bool) -> SessionInfo {
        SessionInfo {
            name: "Cloud".into(),
            tint: [1, 2, 3],
            area_id: AreaId {
                game: if fixture { GameId::Fixture } else { GameId::Ff7 },
                key: "k".into(),
            },
            fixture,
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
}
