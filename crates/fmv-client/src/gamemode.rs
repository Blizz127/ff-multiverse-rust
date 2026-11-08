//! Where the client runs: an ordinary desktop, or Steam's Game Mode (a gamescope session, as on a
//! Steam Deck or a Bazzite console). It decides two things, both once at start-up and from the
//! environment alone:
//!
//! * **Full screen.** The window is a borderless full-screen window when `--fullscreen` is given or
//!   when ANY of these is true ([`Signals::fullscreen`]):
//!   1. `GAMESCOPE_WAYLAND_DISPLAY` is set and not empty;
//!   2. `XDG_CURRENT_DESKTOP`, a `:`-separated list, has an entry equal to `gamescope` (any case);
//!   3. `SteamGamepadUI` is `1`.
//!
//!   Checks 1 and 2 are exported by gamescope itself (its `main.cpp`) to everything it starts, so a
//!   game started from Game Mode, by Steam or by a launcher that Steam started, inherits them.
//!   Check 3 is Steam's: it sets it for games started from its gamepad UI, which is reported to be
//!   Big Picture on an ordinary desktop too, so that alone gets full screen but no keyboard
//!   (below). A normal desktop session (KDE, started from a menu or a terminal) has none of the
//!   three and stays windowed.
//! * **The on-screen keyboard.** Steam's keyboard is asked for only when check 1 or 2 is true, i.e.
//!   inside a gamescope session ([`Signals::gamescope`]), and only when a text field gains focus
//!   (see [`osk_transition`]). On the desktop nothing is ever asked: the call would hand a
//!   `steam://` address to the desktop, which can start Steam.
//!
//! `SteamDeck=1` is deliberately not a check: Steam sets it for the games it starts on Deck
//! hardware, desktop mode is expected to be no exception (not verified here), so on its own it
//! does not mean Game Mode.
//!
//! The environment is read in one place, `main`, through [`Signals::from_env`]; everything else
//! receives the decision ([`DeviceMode`]) as a value, so no test depends on the machine it runs on.
//!
//! # How the keyboard is asked for
//!
//! `steam://open/keyboard` when the field gains focus and `steam://close/keyboard` when it loses it,
//! handed to Steam with `xdg-open` (and, if that fails, `steam -ifrunning <url>`), the same way a
//! shell script would. The keyboard then types key events with their text into the focused game
//! window, which the character menu already takes, so no input-method code is involved.
//!
//! Evidence, stated plainly: Valve documents no such address for games that are not Steam titles
//! (the documented route is the Steamworks call `ShowFloatingGamepadTextInput`, which needs a Steam
//! app id this owner-only port does not have). The addresses come from one third-party report of a
//! test on a Steam Deck in Game Mode, from a non-Steam shortcut, in which both addresses worked and
//! the keys arrived as ordinary key events (Laeresh/CSVM issue 111). It has NOT been run by us on a
//! device: the owner's device check is the first real observation here.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// What the environment says about the session.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Signals {
    /// `GAMESCOPE_WAYLAND_DISPLAY`, when set and not empty (its value is gamescope's socket name).
    pub gamescope_display: Option<String>,
    /// `XDG_CURRENT_DESKTOP`, when one of its `:`-separated entries is `gamescope` (any case).
    pub gamescope_desktop: Option<String>,
    /// `SteamGamepadUI` is `1`: Steam's gamepad UI started this process.
    pub steam_gamepad_ui: bool,
}

impl Signals {
    /// Read the signals through `get`, the environment lookup (`std::env::var(name).ok()` in the
    /// program, a table in tests).
    pub fn from_env(get: impl Fn(&str) -> Option<String>) -> Self {
        let value = |name: &str| get(name).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        Self {
            gamescope_display: value("GAMESCOPE_WAYLAND_DISPLAY"),
            gamescope_desktop: value("XDG_CURRENT_DESKTOP")
                .filter(|list| list.split(':').any(|entry| entry.trim().eq_ignore_ascii_case("gamescope"))),
            steam_gamepad_ui: value("SteamGamepadUI").is_some_and(|v| v == "1"),
        }
    }

    /// Is this a gamescope session? (Checks 1 and 2 above.)
    pub fn gamescope(&self) -> bool {
        self.gamescope_display.is_some() || self.gamescope_desktop.is_some()
    }

    /// Does the environment alone ask for full screen? (Any of the three checks.)
    pub fn fullscreen(&self) -> bool {
        self.gamescope() || self.steam_gamepad_ui
    }
}

/// What this run does about the device: decided once, handed down as a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeviceMode {
    /// Open the window as borderless full screen.
    pub fullscreen: bool,
    /// Ask Steam for its on-screen keyboard when a text field gains focus.
    pub steam_keyboard: bool,
}

impl DeviceMode {
    /// An ordinary desktop: a window, no Steam keyboard. What tests use.
    pub const DESKTOP: DeviceMode = DeviceMode { fullscreen: false, steam_keyboard: false };

    /// Full screen if asked for (`--fullscreen`) or if the environment says so; the keyboard only
    /// inside a gamescope session.
    pub fn decide(fullscreen_flag: bool, signals: &Signals) -> Self {
        Self { fullscreen: fullscreen_flag || signals.fullscreen(), steam_keyboard: signals.gamescope() }
    }
}

/// The start-up line that says what was seen and what it led to, for the owner's device report.
pub fn describe(fullscreen_flag: bool, signals: &Signals, mode: DeviceMode) -> String {
    let mut seen = Vec::new();
    if let Some(display) = &signals.gamescope_display {
        seen.push(format!("GAMESCOPE_WAYLAND_DISPLAY={}", crate::net::clean(display)));
    }
    if let Some(desktop) = &signals.gamescope_desktop {
        seen.push(format!("XDG_CURRENT_DESKTOP={}", crate::net::clean(desktop)));
    }
    if signals.steam_gamepad_ui {
        seen.push("SteamGamepadUI=1".to_string());
    }
    if fullscreen_flag {
        seen.push("--fullscreen".to_string());
    }
    let seen = if seen.is_empty() { "no gamescope, no Steam gamepad UI".to_string() } else { seen.join(", ") };
    format!(
        "device: {seen}; window: {}; on-screen keyboard: {}",
        if mode.fullscreen { "borderless full screen" } else { "windowed" },
        if mode.steam_keyboard { "Steam's, when a text field gets focus" } else { "off" }
    )
}

/// A request for Steam's on-screen keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Osk {
    Open,
    Close,
}

impl Osk {
    pub fn url(self) -> &'static str {
        match self {
            Osk::Open => "steam://open/keyboard",
            Osk::Close => "steam://close/keyboard",
        }
    }
}

/// What a text field gaining or losing the focus asks Steam for (`None`: nothing). Only inside a
/// gamescope session ([`DeviceMode::steam_keyboard`]); on a desktop never.
pub fn osk_transition(mode: DeviceMode, was_focused: bool, is_focused: bool) -> Option<Osk> {
    if !mode.steam_keyboard {
        return None;
    }
    match (was_focused, is_focused) {
        (false, true) => Some(Osk::Open),
        (true, false) => Some(Osk::Close),
        _ => None,
    }
}

/// A program to run and its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spawn {
    pub program: &'static str,
    pub args: Vec<String>,
}

/// The ways to hand `url` to Steam, in the order they are tried: the desktop's own URL opener
/// (`xdg-open` runs whatever is registered for `steam://`, Steam itself), then Steam's launcher
/// told to act only if Steam is already running (so it never starts Steam by itself).
pub fn steam_commands(url: &str) -> Vec<Spawn> {
    vec![
        Spawn { program: "xdg-open", args: vec![url.to_string()] },
        Spawn { program: "steam", args: vec!["-ifrunning".to_string(), url.to_string()] },
    ]
}

/// Runs one command to its end. The seam between the decisions above and the system.
pub trait Runner {
    fn run(&self, spawn: &Spawn) -> Result<(), String>;
}

/// Try [`steam_commands`] for `request` until one works; `Err` lists what failed.
pub fn run_osk(request: Osk, runner: &impl Runner) -> Result<(), String> {
    let mut failures = Vec::new();
    for spawn in steam_commands(request.url()) {
        match runner.run(&spawn) {
            Ok(()) => return Ok(()),
            Err(e) => failures.push(format!("{}: {e}", spawn.program)),
        }
    }
    Err(failures.join("; "))
}

/// How long a helper program may take before it is killed (a hung opener must not pile up).
const SPAWN_LIMIT: Duration = Duration::from_secs(10);

/// Runs the real programs: no input, no output, the environment as it is (the one test of the
/// addresses on a device ran with it), and a time limit.
pub struct SystemRunner {
    pub limit: Duration,
}

impl Default for SystemRunner {
    fn default() -> Self {
        Self { limit: SPAWN_LIMIT }
    }
}

impl Runner for SystemRunner {
    fn run(&self, spawn: &Spawn) -> Result<(), String> {
        let mut child = Command::new(spawn.program)
            .args(&spawn.args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        let deadline = Instant::now() + self.limit;
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => return Ok(()),
                Ok(Some(status)) => return Err(format!("exited with {status}")),
                Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("did not finish in time".to_string());
                }
                Err(e) => return Err(e.to_string()),
            }
        }
    }
}

/// Ask Steam to show or hide its keyboard, off the UI thread (the opener can take a moment); one
/// line on stderr says how it went.
#[cfg(not(test))]
pub fn request_osk(request: Osk) {
    let what = match request {
        Osk::Open => "open",
        Osk::Close => "close",
    };
    let spawned = std::thread::Builder::new().name("steam-keyboard".to_string()).spawn(move || {
        match run_osk(request, &SystemRunner::default()) {
            Ok(()) => eprintln!("keyboard: asked Steam to {what} its on-screen keyboard"),
            Err(e) => eprintln!("keyboard: could not ask Steam to {what} its on-screen keyboard ({e})"),
        }
    });
    if let Err(e) = spawned {
        eprintln!("keyboard: could not start a helper to ask Steam to {what} its on-screen keyboard ({e})");
    }
}

#[cfg(test)]
thread_local! {
    static ASKED: std::cell::RefCell<Vec<Osk>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// In tests nothing is ever started: what would have been asked is recorded for this test's thread
/// ([`take_asked`]), so a test can say what the code asked for and none can reach Steam.
#[cfg(test)]
pub fn request_osk(request: Osk) {
    ASKED.with(|asked| asked.borrow_mut().push(request));
}

/// What [`request_osk`] was asked for on this thread since the last call (tests only).
#[cfg(test)]
pub fn take_asked() -> Vec<Osk> {
    ASKED.with(|asked| std::mem::take(&mut *asked.borrow_mut()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let table: HashMap<String, String> = pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect();
        move |name| table.get(name).cloned()
    }

    fn signals(pairs: &[(&str, &str)]) -> Signals {
        Signals::from_env(env(pairs))
    }

    #[test]
    fn a_gamescope_session_is_told_by_either_of_the_two_variables_gamescope_exports() {
        let s = signals(&[("GAMESCOPE_WAYLAND_DISPLAY", "gamescope-0")]);
        assert!(s.gamescope() && s.fullscreen());
        assert_eq!(s.gamescope_display.as_deref(), Some("gamescope-0"));
        let s = signals(&[("XDG_CURRENT_DESKTOP", "gamescope")]);
        assert!(s.gamescope() && s.fullscreen());
        // The desktop variable is a list, and the case does not matter.
        for list in ["KDE:gamescope", "GAMESCOPE", "x:Gamescope:y", " gamescope "] {
            assert!(signals(&[("XDG_CURRENT_DESKTOP", list)]).gamescope(), "{list:?}");
        }
        // Both, as a real Game Mode session has them.
        let s = signals(&[("GAMESCOPE_WAYLAND_DISPLAY", "gamescope-0"), ("XDG_CURRENT_DESKTOP", "gamescope"), ("SteamGamepadUI", "1")]);
        assert!(s.gamescope() && s.steam_gamepad_ui);
    }

    #[test]
    fn an_ordinary_desktop_is_none_of_it() {
        for desktop in ["KDE", "GNOME", "KDE:plasma", "gamescopes", "notgamescope", ""] {
            let s = signals(&[("XDG_CURRENT_DESKTOP", desktop)]);
            assert!(!s.gamescope() && !s.fullscreen(), "{desktop:?}");
        }
        assert_eq!(signals(&[]), Signals::default());
        // An empty or blank gamescope display is not set.
        for blank in ["", "  "] {
            assert!(!signals(&[("GAMESCOPE_WAYLAND_DISPLAY", blank)]).gamescope(), "{blank:?}");
        }
        // SteamGamepadUI counts only as 1; SteamDeck counts for nothing at all.
        for value in ["0", "", "true", "yes"] {
            assert!(!signals(&[("SteamGamepadUI", value)]).fullscreen(), "{value:?}");
        }
        let deck_desktop_mode = signals(&[("SteamDeck", "1"), ("SteamOS", "1"), ("XDG_CURRENT_DESKTOP", "KDE")]);
        assert!(!deck_desktop_mode.fullscreen() && !deck_desktop_mode.gamescope());
    }

    #[test]
    fn the_decision_full_screen_by_flag_or_environment_the_keyboard_only_in_gamescope() {
        let desktop = signals(&[("XDG_CURRENT_DESKTOP", "KDE")]);
        assert_eq!(DeviceMode::decide(false, &desktop), DeviceMode::DESKTOP);
        assert_eq!(DeviceMode::DESKTOP, DeviceMode::default());
        // --fullscreen on the desktop: full screen, still no Steam keyboard.
        assert_eq!(DeviceMode::decide(true, &desktop), DeviceMode { fullscreen: true, steam_keyboard: false });
        // Game Mode: both, flag or not.
        let game_mode = signals(&[("GAMESCOPE_WAYLAND_DISPLAY", "gamescope-0"), ("XDG_CURRENT_DESKTOP", "gamescope"), ("SteamGamepadUI", "1")]);
        for flag in [false, true] {
            assert_eq!(DeviceMode::decide(flag, &game_mode), DeviceMode { fullscreen: true, steam_keyboard: true });
        }
        // Big Picture on the KDE desktop (Steam's gamepad UI started the game, gamescope did not):
        // full screen, but nothing is ever asked of Steam's keyboard.
        let big_picture = signals(&[("XDG_CURRENT_DESKTOP", "KDE"), ("SteamGamepadUI", "1")]);
        assert_eq!(DeviceMode::decide(false, &big_picture), DeviceMode { fullscreen: true, steam_keyboard: false });
        // Each gamescope signal alone is a gamescope session.
        for pair in [("GAMESCOPE_WAYLAND_DISPLAY", "gamescope-0"), ("XDG_CURRENT_DESKTOP", "gamescope")] {
            assert_eq!(DeviceMode::decide(false, &signals(&[pair])), DeviceMode { fullscreen: true, steam_keyboard: true }, "{pair:?}");
        }
    }

    #[test]
    fn the_start_up_line_says_what_was_seen_and_what_it_led_to() {
        let game_mode = signals(&[("GAMESCOPE_WAYLAND_DISPLAY", "gamescope-0"), ("XDG_CURRENT_DESKTOP", "gamescope"), ("SteamGamepadUI", "1")]);
        let line = describe(false, &game_mode, DeviceMode::decide(false, &game_mode));
        for needle in ["GAMESCOPE_WAYLAND_DISPLAY=gamescope-0", "XDG_CURRENT_DESKTOP=gamescope", "SteamGamepadUI=1", "borderless full screen", "Steam's"] {
            assert!(line.contains(needle), "{needle:?} in {line}");
        }
        let none = Signals::default();
        let line = describe(false, &none, DeviceMode::DESKTOP);
        assert_eq!(line, "device: no gamescope, no Steam gamepad UI; window: windowed; on-screen keyboard: off");
        let line = describe(true, &none, DeviceMode::decide(true, &none));
        assert_eq!(line, "device: --fullscreen; window: borderless full screen; on-screen keyboard: off");
        // Text from the environment is cleaned before it is printed.
        let nasty = signals(&[("GAMESCOPE_WAYLAND_DISPLAY", "gamescope-0\u{1b}[2J")]);
        assert!(!describe(false, &nasty, DeviceMode::decide(false, &nasty)).contains('\u{1b}'));
    }

    #[test]
    fn a_text_field_asks_for_the_keyboard_when_it_gains_focus_and_lets_it_go_when_it_loses_it_but_only_in_gamescope() {
        let game = DeviceMode { fullscreen: true, steam_keyboard: true };
        assert_eq!(osk_transition(game, false, true), Some(Osk::Open));
        assert_eq!(osk_transition(game, true, false), Some(Osk::Close));
        assert_eq!(osk_transition(game, true, true), None, "staying in the field asks nothing more");
        assert_eq!(osk_transition(game, false, false), None);
        // Desktop, windowed or full screen: never.
        for mode in [DeviceMode::DESKTOP, DeviceMode { fullscreen: true, steam_keyboard: false }] {
            for (was, is) in [(false, true), (true, false), (true, true), (false, false)] {
                assert_eq!(osk_transition(mode, was, is), None, "{mode:?} {was} -> {is}");
            }
        }
    }

    #[test]
    fn the_addresses_and_the_commands_that_carry_them() {
        assert_eq!(Osk::Open.url(), "steam://open/keyboard");
        assert_eq!(Osk::Close.url(), "steam://close/keyboard");
        let commands = steam_commands(Osk::Open.url());
        assert_eq!(
            commands,
            [
                Spawn { program: "xdg-open", args: vec!["steam://open/keyboard".to_string()] },
                Spawn { program: "steam", args: vec!["-ifrunning".to_string(), "steam://open/keyboard".to_string()] },
            ]
        );
    }

    /// A runner that runs nothing: it records what it was asked and answers from a script.
    struct Script {
        answers: RefCell<Vec<Result<(), String>>>,
        asked: RefCell<Vec<Spawn>>,
    }

    impl Script {
        fn new(answers: Vec<Result<(), String>>) -> Self {
            Self { answers: RefCell::new(answers), asked: RefCell::new(Vec::new()) }
        }
    }

    impl Runner for Script {
        fn run(&self, spawn: &Spawn) -> Result<(), String> {
            self.asked.borrow_mut().push(spawn.clone());
            self.answers.borrow_mut().remove(0)
        }
    }

    #[test]
    fn the_first_command_that_works_ends_it_and_a_failure_falls_back_to_the_next() {
        let runner = Script::new(vec![Ok(())]);
        assert_eq!(run_osk(Osk::Open, &runner), Ok(()));
        assert_eq!(runner.asked.borrow().len(), 1);
        assert_eq!(runner.asked.borrow()[0].program, "xdg-open");

        let runner = Script::new(vec![Err("No such file or directory".to_string()), Ok(())]);
        assert_eq!(run_osk(Osk::Close, &runner), Ok(()));
        let asked = runner.asked.borrow();
        assert_eq!(asked.iter().map(|s| s.program).collect::<Vec<_>>(), ["xdg-open", "steam"]);
        assert_eq!(asked[1].args.last().map(String::as_str), Some("steam://close/keyboard"));
        drop(asked);

        let runner = Script::new(vec![Err("exited with 3".to_string()), Err("exited with 1".to_string())]);
        let error = run_osk(Osk::Open, &runner).unwrap_err();
        assert_eq!(error, "xdg-open: exited with 3; steam: exited with 1");
    }

    /// The real runner, on programs that cannot do anything (never `xdg-open` or `steam`).
    #[test]
    fn the_system_runner_reports_success_failure_a_missing_program_and_a_hang() {
        let run = |program: &'static str, args: &[&str], limit: Duration| {
            SystemRunner { limit }.run(&Spawn { program, args: args.iter().map(|a| (*a).to_string()).collect() })
        };
        let long = Duration::from_secs(5);
        assert_eq!(run("true", &[], long), Ok(()));
        let failed = run("false", &[], long).unwrap_err();
        assert!(failed.starts_with("exited with"), "{failed}");
        assert!(run("fmv-no-such-program-anywhere", &[], long).is_err());
        // A program that does not finish is killed at the limit and reported, not waited for.
        let started = Instant::now();
        assert_eq!(run("sleep", &["30"], Duration::from_millis(200)), Err("did not finish in time".to_string()));
        assert!(started.elapsed() < Duration::from_secs(5), "it was killed, not waited for");
        assert_eq!(SystemRunner::default().limit, SPAWN_LIMIT);
    }

    #[test]
    fn in_tests_a_request_is_recorded_and_nothing_is_started() {
        let _ = take_asked();
        request_osk(Osk::Open);
        request_osk(Osk::Close);
        assert_eq!(take_asked(), [Osk::Open, Osk::Close]);
        assert!(take_asked().is_empty(), "taking them empties the record");
    }
}
