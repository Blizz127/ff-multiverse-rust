//! Window mode: winit 0.30 `ApplicationHandler` + the wgpu renderer.
//!
//! The event loop is created before the session starts (see `main`), so a missing display is
//! reported cleanly before any network traffic. Every start-up failure (window, GPU) becomes an
//! error message and a non-zero exit code, never a panic.

use crate::cli::exit;
use crate::interp::render_time;
use crate::render::{FrameResult, PlayerSprite, Renderer, Scene};
use crate::session::{Session, SessionInfo};
use fmv_adapter_api::Bounds;
use fmv_core::Pos;
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

pub fn run(event_loop: EventLoop<()>, session: Session) -> u8 {
    let mut app = App::new(session);
    // A panic inside the windowing or GPU stack must not skip the goodbye or end as a raw abort.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| event_loop.run_app(&mut app)));
    app.leave(); // no-op when the loop already said goodbye
    match (app.failure.take(), result) {
        (Some(msg), _) => {
            eprintln!("error: {msg}");
            exit::FAILURE
        }
        (None, Err(payload)) => {
            eprintln!("error: the window loop crashed: {}", panic_text(payload.as_ref()));
            exit::FAILURE
        }
        (None, Ok(Err(e))) => {
            eprintln!("error: the window event loop failed: {e}");
            exit::FAILURE
        }
        (None, Ok(Ok(()))) => exit::OK,
    }
}

/// Window title. Fixture areas always say FIXTURE so they are never mistaken for the real game.
pub fn base_title(info: &SessionInfo) -> String {
    if info.fixture {
        format!("FF Multiverse - FIXTURE (synthetic, not FF7) - {}", info.name)
    } else {
        format!("FF Multiverse - {} - {}", info.area_id, info.name)
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

struct App {
    session: Session,
    base_title: String,
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    input: Input,
    triangles: Vec<[Pos; 3]>,
    bounds: Bounds,
    last_tick: Instant,
    last_title: Instant,
    left: bool,
    failure: Option<String>,
}

impl App {
    fn new(session: Session) -> Self {
        let triangles = session.area.walk_triangles();
        let bounds = session.area.bounds();
        let base_title = base_title(&session.info);
        let now = Instant::now();
        Self {
            session,
            base_title,
            window: None,
            renderer: None,
            input: Input::default(),
            triangles,
            bounds,
            last_tick: now,
            last_title: now.checked_sub(TITLE_REFRESH).unwrap_or(now),
            left: false,
            failure: None,
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
        if self.last_title.elapsed() < TITLE_REFRESH {
            return;
        }
        self.last_title = Instant::now();
        let rtt = self
            .session
            .state
            .last_rtt()
            .map_or_else(|| "-".to_string(), |d| format!("{} ms", d.as_millis()));
        window.set_title(&format!(
            "{} | players {} | rtt {rtt}",
            self.base_title,
            self.session.state.remote_count() + 1
        ));
    }

    fn tick(&mut self, el: &ActiveEventLoop) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f32().min(MAX_DT);
        self.last_tick = now;
        if let Some(reason) = self.session.pump() {
            self.fail(el, reason);
            return;
        }
        let speed = WALK_SPEED_FACTOR * self.session.area.max_speed();
        self.session
            .state
            .own
            .walk(self.session.area.as_ref(), self.input.dir(), speed, dt);
        self.session.flush_intent();
        self.update_title();
    }

    fn redraw(&mut self, el: &ActiveEventLoop) {
        let Some(renderer) = self.renderer.as_mut() else { return };
        let now = self.session.now();
        let mut players: Vec<PlayerSprite> = self
            .session
            .state
            .remote_views(render_time(now))
            .iter()
            .map(|v| PlayerSprite {
                x: v.pose.pos.x,
                y: v.pose.pos.y,
                tint: v.tint,
                own: false,
            })
            .collect();
        let own = self.session.state.own.pos();
        players.push(PlayerSprite {
            x: own.x,
            y: own.y,
            tint: self.session.info.tint,
            own: true,
        });
        let scene = Scene {
            bounds: self.bounds,
            triangles: &self.triangles,
            players: &players,
            fixture_banner: self.session.info.fixture,
        };
        let result = renderer.render(&scene);
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
        let attrs = Window::default_attributes()
            .with_title(self.base_title.clone())
            .with_inner_size(LogicalSize::new(1100.0, 680.0))
            .with_min_inner_size(LogicalSize::new(320.0, 240.0));
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
                        ..
                    },
                ..
            } => {
                let pressed = state == ElementState::Pressed;
                if code == KeyCode::Escape {
                    if pressed {
                        self.quit(el);
                    }
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
    use fmv_core::{AreaId, GameId};

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
}
