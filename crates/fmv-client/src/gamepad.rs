//! Game controllers: gilrs events turned into the keys the keyboard path already handles.
//!
//! There is no second input path. A controller produces [`PadEvent`]s, each one a press or a release
//! of a [`PadKey`], and a `PadKey` is only a name for a keyboard key the client already knows
//! ([`PadKey::key_code`]): the window feeds it to the same `route_key` dispatch as a key of the
//! keyboard, and the character menu turns it into the same `MenuInput` a key press makes
//! (`ui::input::pad_menu_input`).
//!
//! # The map
//!
//! Names are gilrs's (SDL's), so they are the same whatever the pad and whatever Steam Input
//! presents: *south* is the Xbox A, *east* B, *west* X, *north* Y.
//!
//! | Controller | Key | In the field | In a battle | In the character menu |
//! |---|---|---|---|---|
//! | left stick, D-pad | arrows | walk (eight ways) | menu cursor and target pointer | move, change a value |
//! | A (south) | Enter | - | select / confirm | play, create, answer, dismiss a notice |
//! | B (east) | Backspace | - | back a level | back / cancel (never quits) |
//! | X (west) | P | invite the nearest player | (same) | new character on the list, Backspace on the form |
//! | Y (north) | Y | accept the newest invite | (same) | delete a saved character (asks first: Keep, Delete or Forget here) |
//! | Select / View | L | leave the party | (same) | - |
//! | Start / Menu | Esc | quit prompt: a second press quits | back a level, never quits | Esc: back, cancel, or quit |
//! | right bumper (R1/RB) | B | ask for a debug battle | - | - |
//! | left trigger (L2/LT), held | Shift | walk instead of run | - | - |
//!
//! Everything else (the Guide/Steam button, the stick clicks, L1, R2, the right stick) does nothing.
//!
//! # Sticks, repeat, focus
//!
//! * A stick axis counts as pressed past [`STICK_PRESS`] of its travel and as let go below
//!   [`STICK_RELEASE`], so a stick resting near the line does not chatter. In the field the stick
//!   reads in eight directions (diagonals walk); in menus and battles only the stronger axis counts,
//!   so a slightly crooked push never moves the cursor and a slider at once ([`PadMode`]).
//! * In menus and battles a held direction repeats like a held arrow key: after [`REPEAT_DELAY`],
//!   then every [`REPEAT_INTERVAL`]. No other button repeats (a held A confirms once).
//! * Controllers are only listened to while the window has the focus; losing it lets go of
//!   everything held, and what is still held when the focus returns waits for a fresh press.
//! * Hot-plug is gilrs's: pads can come and go at any time, and one that goes lets go of what it held.
//!   Several pads at once are one: a key is held while any of them holds it.
//! * No controller (or no way to read them) changes nothing: [`Pads::poll`] then returns no events.
//!
//! The decisions above are pure ([`button_key`], [`stick_keys`], [`PadLogic`]) and tested without a
//! device; [`Pads`] is the thin layer that reads gilrs.

use gilrs::{Axis, Button, EventType, Gamepad, Gilrs, GilrsBuilder, MappingSource};
use std::collections::BTreeMap;
use std::ops::{BitAnd, BitOr, Not};
use std::time::{Duration, Instant};
use winit::keyboard::KeyCode;

/// A stick axis is pressed once it is pushed this far (of its full travel, 0.0 to 1.0).
pub const STICK_PRESS: f32 = 0.6;
/// A pressed stick axis is let go once it falls back below this.
pub const STICK_RELEASE: f32 = 0.4;
/// A held direction starts repeating after this long (menus and battles).
pub const REPEAT_DELAY: Duration = Duration::from_millis(400);
/// A repeating direction repeats this often (about 8 a second: a held stick can be stopped on the
/// entry it wants, where a keyboard's 25 a second would run past it).
pub const REPEAT_INTERVAL: Duration = Duration::from_millis(120);
/// The most events one poll reads from gilrs, so a pad that floods cannot stall a frame.
const MAX_EVENTS_PER_POLL: usize = 1024;

/// A key a controller can press: a keyboard key the client already knows, named by its job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PadKey {
    // The four directions come first: `PadKey::DIRECTIONS` is the first four of `ALL`.
    Up,
    Down,
    Left,
    Right,
    /// A: Enter.
    Confirm,
    /// B: Backspace.
    Back,
    /// X: P, invite the nearest player.
    Invite,
    /// Y: Y, accept the newest invite.
    Accept,
    /// Select: L, leave the party.
    Leave,
    /// The right bumper: B, ask for a debug battle.
    Battle,
    /// Start: Esc.
    Escape,
    /// The left trigger, held: Shift, walk instead of run.
    Walk,
}

impl PadKey {
    /// Every key, in the order events are produced.
    pub const ALL: [PadKey; 12] = [
        PadKey::Up,
        PadKey::Down,
        PadKey::Left,
        PadKey::Right,
        PadKey::Confirm,
        PadKey::Back,
        PadKey::Invite,
        PadKey::Accept,
        PadKey::Leave,
        PadKey::Battle,
        PadKey::Escape,
        PadKey::Walk,
    ];
    /// The four directions: the only keys that repeat.
    pub const DIRECTIONS: [PadKey; 4] = [PadKey::Up, PadKey::Down, PadKey::Left, PadKey::Right];

    fn bit(self) -> u16 {
        1 << (self as u8)
    }

    /// The keyboard key this stands for in the game window (`route_key`'s input).
    ///
    /// Walking uses the RIGHT Shift key so that a keyboard's left Shift and the pad's trigger do
    /// not let go of each other (the window keeps one flag per Shift key).
    pub fn key_code(self) -> KeyCode {
        match self {
            PadKey::Up => KeyCode::ArrowUp,
            PadKey::Down => KeyCode::ArrowDown,
            PadKey::Left => KeyCode::ArrowLeft,
            PadKey::Right => KeyCode::ArrowRight,
            PadKey::Confirm => KeyCode::Enter,
            PadKey::Back => KeyCode::Backspace,
            PadKey::Invite => KeyCode::KeyP,
            PadKey::Accept => KeyCode::KeyY,
            PadKey::Leave => KeyCode::KeyL,
            PadKey::Battle => KeyCode::KeyB,
            PadKey::Escape => KeyCode::Escape,
            PadKey::Walk => KeyCode::ShiftRight,
        }
    }
}

/// The key a controller button stands for (`None`: the button does nothing). Names are gilrs's.
pub fn button_key(button: Button) -> Option<PadKey> {
    Some(match button {
        Button::DPadUp => PadKey::Up,
        Button::DPadDown => PadKey::Down,
        Button::DPadLeft => PadKey::Left,
        Button::DPadRight => PadKey::Right,
        Button::South => PadKey::Confirm,
        Button::East => PadKey::Back,
        Button::West => PadKey::Invite,
        Button::North => PadKey::Accept,
        Button::Select => PadKey::Leave,
        Button::Start => PadKey::Escape,
        Button::RightTrigger => PadKey::Battle,
        Button::LeftTrigger2 => PadKey::Walk,
        _ => return None,
    })
}

/// A set of [`PadKey`]s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct KeySet(u16);

impl KeySet {
    const EMPTY: KeySet = KeySet(0);

    fn has(self, key: PadKey) -> bool {
        self.0 & key.bit() != 0
    }

    fn with(self, key: PadKey) -> KeySet {
        KeySet(self.0 | key.bit())
    }

    fn without(self, key: PadKey) -> KeySet {
        KeySet(self.0 & !key.bit())
    }
}

impl BitOr for KeySet {
    type Output = KeySet;
    fn bitor(self, other: KeySet) -> KeySet {
        KeySet(self.0 | other.0)
    }
}

impl BitAnd for KeySet {
    type Output = KeySet;
    fn bitand(self, other: KeySet) -> KeySet {
        KeySet(self.0 & other.0)
    }
}

impl Not for KeySet {
    type Output = KeySet;
    fn not(self) -> KeySet {
        KeySet(!self.0)
    }
}

/// The directions a left stick at (`x`, `y`) holds, given the ones it held a moment ago. `x` is
/// positive to the right and `y` positive upwards (gilrs makes it so on every platform); both are
/// in -1.0 to 1.0.
///
/// An axis presses at [`STICK_PRESS`] and lets go at [`STICK_RELEASE`]. With `diagonals` a push
/// toward a corner holds two directions; without, only the axis pushed harder counts, and the one
/// that was held stays unless the other is clearly stronger (a quarter more), so a stick that
/// wobbles around the diagonal does not flip between them.
fn stick_keys(x: f32, y: f32, held: KeySet, diagonals: bool) -> KeySet {
    // Anything that is not a number (a broken driver) is a stick at rest.
    let (x, y) = (if x.is_finite() { x } else { 0.0 }, if y.is_finite() { y } else { 0.0 });
    let pushed = |value: f32, key: PadKey| value >= if held.has(key) { STICK_RELEASE } else { STICK_PRESS };
    let mut keys = KeySet::EMPTY;
    for (value, key) in [(y, PadKey::Up), (-y, PadKey::Down), (-x, PadKey::Left), (x, PadKey::Right)] {
        if pushed(value, key) {
            keys = keys.with(key);
        }
    }
    let vertical = keys.has(PadKey::Up) || keys.has(PadKey::Down);
    let horizontal = keys.has(PadKey::Left) || keys.has(PadKey::Right);
    if diagonals || !(vertical && horizontal) {
        return keys;
    }
    let was_vertical = held.has(PadKey::Up) || held.has(PadKey::Down);
    let was_horizontal = held.has(PadKey::Left) || held.has(PadKey::Right);
    let (ax, ay) = (x.abs(), y.abs());
    let horizontal_wins = match (was_horizontal, was_vertical) {
        (true, false) => ax * 1.25 >= ay,
        (false, true) => ax > ay * 1.25,
        _ => ax >= ay,
    };
    let loser: [PadKey; 2] = if horizontal_wins { [PadKey::Up, PadKey::Down] } else { [PadKey::Left, PadKey::Right] };
    loser.into_iter().fold(keys, KeySet::without)
}

/// One press or release of a [`PadKey`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PadEvent {
    pub key: PadKey,
    pub pressed: bool,
    /// The repeat of a held direction (only ever with `pressed`), like a keyboard's auto-repeat.
    pub repeat: bool,
}

/// What the window is doing with the controllers' directions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PadMode {
    /// Walking in a field: the stick reads in eight directions and a held direction is just held
    /// (the window walks as long as the key is down), so nothing repeats.
    Field,
    /// A menu or a battle: the stick reads in four directions and a held direction repeats.
    Menu,
}

/// What one input did, as gilrs reports it, reduced to what this module uses. `Button` and `Axis`
/// are gilrs's plain enums, so the pure code is tested with them and no device.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RawInput {
    Button(Button, bool),
    Axis(Axis, f32),
    /// The pad was unplugged: it holds nothing any more.
    Gone,
}

/// What one pad holds right now.
#[derive(Debug, Default)]
struct PadState {
    /// Keys held by buttons (the D-pad included).
    buttons: KeySet,
    /// The left stick: `x` to the right, `y` upwards.
    stick: (f32, f32),
    /// The directions the stick is holding (also the memory of its hysteresis).
    stick_keys: KeySet,
}

/// The pure heart: raw pad inputs in, [`PadEvent`]s out.
#[derive(Debug)]
pub struct PadLogic {
    pads: BTreeMap<usize, PadState>,
    /// What the consumers have been told is held.
    held: KeySet,
    /// Is the window focused? Unfocused, nothing is held (everything was released).
    enabled: bool,
    /// Keys that were already down when the focus came back: ignored until they are let go.
    suppressed: KeySet,
    /// What the pads held at the last update, focus or not.
    last_raw: KeySet,
    /// When each direction (in [`PadKey::DIRECTIONS`] order) repeats next.
    repeat_due: [Option<Instant>; 4],
}

impl Default for PadLogic {
    fn default() -> Self {
        Self {
            pads: BTreeMap::new(),
            held: KeySet::EMPTY,
            enabled: true,
            suppressed: KeySet::EMPTY,
            last_raw: KeySet::EMPTY,
            repeat_due: [None; 4],
        }
    }
}

impl PadLogic {
    /// Take in one input of pad `pad` (gilrs's id for it).
    pub fn feed(&mut self, pad: usize, input: RawInput) {
        match input {
            RawInput::Button(button, pressed) => {
                if let Some(key) = button_key(button) {
                    let state = self.pads.entry(pad).or_default();
                    state.buttons = if pressed { state.buttons.with(key) } else { state.buttons.without(key) };
                }
            }
            RawInput::Axis(axis, value) => {
                let value = if value.is_finite() { value.clamp(-1.0, 1.0) } else { 0.0 };
                match axis {
                    Axis::LeftStickX => self.pads.entry(pad).or_default().stick.0 = value,
                    Axis::LeftStickY => self.pads.entry(pad).or_default().stick.1 = value,
                    _ => {}
                }
            }
            RawInput::Gone => {
                self.pads.remove(&pad);
            }
        }
    }

    /// The window gained or lost the focus. Losing it lets go of everything at the next update;
    /// regaining it ignores whatever is held at that moment until it is let go.
    pub fn set_focused(&mut self, focused: bool) {
        if focused && !self.enabled {
            self.suppressed = self.last_raw;
        }
        self.enabled = focused;
    }

    /// The events since the last update: first the releases, then the presses (so a change of
    /// direction reads as "let go of up, press down"), then the repeats of held directions.
    pub fn update(&mut self, now: Instant, mode: PadMode) -> Vec<PadEvent> {
        let diagonals = mode == PadMode::Field;
        let mut raw = KeySet::EMPTY;
        for pad in self.pads.values_mut() {
            pad.stick_keys = stick_keys(pad.stick.0, pad.stick.1, pad.stick_keys, diagonals);
            raw = raw | pad.buttons | pad.stick_keys;
        }
        self.last_raw = raw;
        self.suppressed = self.suppressed & raw;
        let target = if self.enabled { raw & !self.suppressed } else { KeySet::EMPTY };

        let mut events = Vec::new();
        for key in PadKey::ALL {
            if self.held.has(key) && !target.has(key) {
                events.push(PadEvent { key, pressed: false, repeat: false });
            }
        }
        for key in PadKey::ALL {
            if !self.held.has(key) && target.has(key) {
                events.push(PadEvent { key, pressed: true, repeat: false });
            }
        }
        self.held = target;

        // The repeat clock of a direction runs while it is held, in every mode, and only menus
        // listen to it: a stick held through the start of a battle repeats within a beat, like a
        // held arrow key does.
        for (due, key) in self.repeat_due.iter_mut().zip(PadKey::DIRECTIONS) {
            if !target.has(key) {
                *due = None;
                continue;
            }
            match *due {
                None => *due = Some(now + REPEAT_DELAY),
                Some(at) if now >= at => {
                    *due = Some(now + REPEAT_INTERVAL);
                    if mode == PadMode::Menu {
                        events.push(PadEvent { key, pressed: true, repeat: true });
                    }
                }
                Some(_) => {}
            }
        }
        events
    }
}

/// The controllers, read through gilrs. Closed (no gilrs behind it) it never has an event.
#[derive(Default)]
pub struct Pads {
    gilrs: Option<Gilrs>,
    logic: PadLogic,
}

impl Pads {
    /// No controller support: nothing is ever read. What tests and the headless modes have.
    pub fn closed() -> Self {
        Self::default()
    }

    /// Start reading controllers. On a machine where that cannot be done (no udev, no permission to
    /// read `/dev/input`) it says so once on stderr and the client goes on with the keyboard.
    ///
    /// Force feedback is off (nothing here rumbles, and it would start a thread of its own); the
    /// mappings of `SDL_GAMECONTROLLERCONFIG`, which Steam sets for the games it starts, are kept.
    pub fn open() -> Self {
        match GilrsBuilder::new().with_force_feedback(false).build() {
            Ok(gilrs) => {
                let mut count = 0;
                for (_, gamepad) in gilrs.gamepads() {
                    count += 1;
                    eprintln!("gamepad: found {}", describe(&gamepad));
                }
                if count == 0 {
                    eprintln!("gamepad: ready, no controller connected (they can be plugged in at any time)");
                }
                Self { gilrs: Some(gilrs), logic: PadLogic::default() }
            }
            Err(e) => {
                eprintln!("gamepad: unavailable ({e}); the keyboard still works");
                Self::closed()
            }
        }
    }

    /// The window gained or lost the focus (see [`PadLogic::set_focused`]).
    pub fn set_focused(&mut self, focused: bool) {
        self.logic.set_focused(focused);
    }

    /// Read what the controllers did since the last call and return the key events it comes to.
    /// Never blocks.
    pub fn poll(&mut self, now: Instant, mode: PadMode) -> Vec<PadEvent> {
        if let Some(gilrs) = self.gilrs.as_mut() {
            for _ in 0..MAX_EVENTS_PER_POLL {
                let Some(event) = gilrs.next_event() else { break };
                let pad = usize::from(event.id);
                match event.event {
                    EventType::Connected => eprintln!("gamepad: connected {}", describe(&gilrs.gamepad(event.id))),
                    EventType::Disconnected => {
                        eprintln!("gamepad: disconnected {}", crate::net::clean(gilrs.gamepad(event.id).name()));
                        self.logic.feed(pad, RawInput::Gone);
                    }
                    EventType::ButtonPressed(button, _) => self.logic.feed(pad, RawInput::Button(button, true)),
                    EventType::ButtonReleased(button, _) => self.logic.feed(pad, RawInput::Button(button, false)),
                    EventType::AxisChanged(axis, value, _) => self.logic.feed(pad, RawInput::Axis(axis, value)),
                    _ => {}
                }
            }
        }
        self.logic.update(now, mode)
    }
}

/// A controller in one line for the log: its name, its USB ids and where its button names come
/// from (a controller gilrs has no names for does nothing useful, and the line says so).
fn describe(gamepad: &Gamepad<'_>) -> String {
    let ids = match (gamepad.vendor_id(), gamepad.product_id()) {
        (Some(vendor), Some(product)) => format!("{vendor:04x}:{product:04x}"),
        _ => "no usb id".to_string(),
    };
    // Without an SDL mapping the buttons are named after the driver's codes, and the Linux xpad
    // driver is reported to send the Xbox X as the "north" code and the Y as "west" (against the
    // kernel's own positional rule): there X and Y would come out swapped.
    let mapping = match gamepad.mapping_source() {
        MappingSource::SdlMappings => "SDL mapping",
        MappingSource::Driver => "no SDL mapping, the driver's own layout (X and Y may be swapped)",
        MappingSource::None => "NO BUTTON MAPPING: its buttons will do nothing",
    };
    format!("\"{}\" ({ids}; {mapping})", crate::net::clean(gamepad.name()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// `(key, pressed, repeat)` of each event, to compare against a list.
    fn flat(events: Vec<PadEvent>) -> Vec<(PadKey, bool, bool)> {
        events.into_iter().map(|e| (e.key, e.pressed, e.repeat)).collect()
    }

    fn press(logic: &mut PadLogic, pad: usize, button: Button) {
        logic.feed(pad, RawInput::Button(button, true));
    }

    fn release(logic: &mut PadLogic, pad: usize, button: Button) {
        logic.feed(pad, RawInput::Button(button, false));
    }

    fn stick(logic: &mut PadLogic, pad: usize, x: f32, y: f32) {
        logic.feed(pad, RawInput::Axis(Axis::LeftStickX, x));
        logic.feed(pad, RawInput::Axis(Axis::LeftStickY, y));
    }

    // ---- the map -------------------------------------------------------------------------------

    #[test]
    fn the_buttons_map_as_documented() {
        for (button, key) in [
            (Button::DPadUp, PadKey::Up),
            (Button::DPadDown, PadKey::Down),
            (Button::DPadLeft, PadKey::Left),
            (Button::DPadRight, PadKey::Right),
            (Button::South, PadKey::Confirm),
            (Button::East, PadKey::Back),
            (Button::West, PadKey::Invite),
            (Button::North, PadKey::Accept),
            (Button::Select, PadKey::Leave),
            (Button::Start, PadKey::Escape),
            (Button::RightTrigger, PadKey::Battle),
            (Button::LeftTrigger2, PadKey::Walk),
        ] {
            assert_eq!(button_key(button), Some(key), "{button:?}");
        }
        // Everything else does nothing: Steam owns the Guide button, the rest is unclaimed.
        for button in [
            Button::Mode,
            Button::LeftThumb,
            Button::RightThumb,
            Button::LeftTrigger,
            Button::RightTrigger2,
            Button::C,
            Button::Z,
            Button::Unknown,
        ] {
            assert_eq!(button_key(button), None, "{button:?}");
        }
    }

    #[test]
    fn every_key_is_a_keyboard_key_the_game_already_knows_and_no_two_share_one() {
        let codes: Vec<KeyCode> = PadKey::ALL.iter().map(|k| k.key_code()).collect();
        assert_eq!(
            codes,
            [
                KeyCode::ArrowUp,
                KeyCode::ArrowDown,
                KeyCode::ArrowLeft,
                KeyCode::ArrowRight,
                KeyCode::Enter,
                KeyCode::Backspace,
                KeyCode::KeyP,
                KeyCode::KeyY,
                KeyCode::KeyL,
                KeyCode::KeyB,
                KeyCode::Escape,
                KeyCode::ShiftRight,
            ]
        );
        for (i, a) in codes.iter().enumerate() {
            assert!(!codes[i + 1..].contains(a), "{a:?} twice");
        }
        // The four directions are the first four keys, which is what the repeat clocks rely on.
        assert_eq!(PadKey::ALL[..4], PadKey::DIRECTIONS);
        // Every key has its own bit.
        let mut seen = 0_u16;
        for key in PadKey::ALL {
            assert_eq!(seen & key.bit(), 0, "{key:?}");
            seen |= key.bit();
        }
    }

    // ---- the stick -------------------------------------------------------------------------------

    fn dirs(keys: KeySet) -> Vec<PadKey> {
        PadKey::DIRECTIONS.into_iter().filter(|k| keys.has(*k)).collect()
    }

    #[test]
    fn a_stick_presses_past_the_threshold_and_lets_go_well_below_it() {
        let none = KeySet::EMPTY;
        // Not pushed far enough: nothing, in either mode.
        assert!(dirs(stick_keys(0.59, 0.0, none, true)).is_empty());
        assert!(dirs(stick_keys(0.0, -0.3, none, false)).is_empty());
        // Pushed: the matching direction (y is up).
        assert_eq!(dirs(stick_keys(0.6, 0.0, none, true)), [PadKey::Right]);
        assert_eq!(dirs(stick_keys(-1.0, 0.0, none, true)), [PadKey::Left]);
        assert_eq!(dirs(stick_keys(0.0, 0.9, none, true)), [PadKey::Up]);
        assert_eq!(dirs(stick_keys(0.0, -0.9, none, true)), [PadKey::Down]);
        // Hysteresis: once held, it stays down until it falls under the release threshold.
        let held = stick_keys(0.8, 0.0, none, true);
        assert_eq!(dirs(stick_keys(0.5, 0.0, held, true)), [PadKey::Right], "0.5 is below the press line but above the release line");
        assert!(dirs(stick_keys(0.39, 0.0, held, true)).is_empty());
        // ...while a stick that was at rest needs the full push again.
        assert!(dirs(stick_keys(0.5, 0.0, none, true)).is_empty());
    }

    #[test]
    fn broken_values_are_a_stick_at_rest() {
        let held = KeySet::EMPTY.with(PadKey::Up).with(PadKey::Right);
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(dirs(stick_keys(bad, bad, held, true)).is_empty(), "{bad}");
            assert!(dirs(stick_keys(bad, 0.0, KeySet::EMPTY, false)).is_empty(), "{bad}");
        }
    }

    #[test]
    fn in_the_field_a_corner_is_two_directions_and_in_menus_only_the_stronger_axis_counts() {
        let none = KeySet::EMPTY;
        assert_eq!(dirs(stick_keys(0.75, 0.75, none, true)), [PadKey::Up, PadKey::Right]);
        assert_eq!(dirs(stick_keys(-0.75, -0.75, none, true)), [PadKey::Down, PadKey::Left]);
        // Menus: the same push is one direction, the stronger one.
        assert_eq!(dirs(stick_keys(0.9, 0.7, none, false)), [PadKey::Right]);
        assert_eq!(dirs(stick_keys(0.7, -0.9, none, false)), [PadKey::Down]);
        assert_eq!(dirs(stick_keys(0.8, 0.8, none, false)).len(), 1, "a dead heat still picks one");
        // The one that was held stays through a wobble around the diagonal...
        let held = stick_keys(0.9, 0.65, none, false);
        assert_eq!(dirs(held), [PadKey::Right]);
        assert_eq!(dirs(stick_keys(0.7, 0.8, held, false)), [PadKey::Right], "up is stronger, but not by a quarter");
        // ...and gives way when the other is clearly stronger.
        assert_eq!(dirs(stick_keys(0.5, 0.9, held, false)), [PadKey::Up]);
    }

    // ---- the state machine -------------------------------------------------------------------------

    #[test]
    fn a_button_press_and_release_are_one_event_each() {
        let (mut logic, now) = (PadLogic::default(), t0());
        assert!(logic.update(now, PadMode::Field).is_empty(), "nothing happened");
        press(&mut logic, 0, Button::South);
        assert_eq!(flat(logic.update(now, PadMode::Field)), [(PadKey::Confirm, true, false)]);
        assert!(logic.update(now + ms(16), PadMode::Field).is_empty(), "held: no new event");
        release(&mut logic, 0, Button::South);
        assert_eq!(flat(logic.update(now + ms(32), PadMode::Field)), [(PadKey::Confirm, false, false)]);
        assert!(logic.update(now + ms(48), PadMode::Field).is_empty());
        // A button that does nothing makes nothing.
        press(&mut logic, 0, Button::Mode);
        press(&mut logic, 0, Button::LeftThumb);
        assert!(logic.update(now + ms(64), PadMode::Field).is_empty());
    }

    #[test]
    fn a_change_of_direction_lets_go_before_it_presses() {
        let (mut logic, now) = (PadLogic::default(), t0());
        stick(&mut logic, 0, 0.0, 1.0);
        assert_eq!(flat(logic.update(now, PadMode::Field)), [(PadKey::Up, true, false)]);
        stick(&mut logic, 0, 0.0, -1.0);
        assert_eq!(
            flat(logic.update(now + ms(16), PadMode::Field)),
            [(PadKey::Up, false, false), (PadKey::Down, true, false)]
        );
        stick(&mut logic, 0, 0.0, 0.0);
        assert_eq!(flat(logic.update(now + ms(32), PadMode::Field)), [(PadKey::Down, false, false)]);
    }

    #[test]
    fn the_dpad_and_the_stick_are_the_same_directions() {
        let (mut logic, now) = (PadLogic::default(), t0());
        press(&mut logic, 0, Button::DPadLeft);
        stick(&mut logic, 0, -1.0, 0.0);
        assert_eq!(flat(logic.update(now, PadMode::Field)), [(PadKey::Left, true, false)], "one press, not two");
        release(&mut logic, 0, Button::DPadLeft);
        assert!(logic.update(now + ms(16), PadMode::Field).is_empty(), "the stick still holds it");
        stick(&mut logic, 0, 0.0, 0.0);
        assert_eq!(flat(logic.update(now + ms(32), PadMode::Field)), [(PadKey::Left, false, false)]);
    }

    #[test]
    fn two_controllers_are_one_and_a_key_is_held_while_either_holds_it() {
        let (mut logic, now) = (PadLogic::default(), t0());
        press(&mut logic, 0, Button::South);
        press(&mut logic, 1, Button::South);
        assert_eq!(flat(logic.update(now, PadMode::Field)), [(PadKey::Confirm, true, false)]);
        release(&mut logic, 0, Button::South);
        assert!(logic.update(now + ms(16), PadMode::Field).is_empty(), "the other pad still holds it");
        release(&mut logic, 1, Button::South);
        assert_eq!(flat(logic.update(now + ms(32), PadMode::Field)), [(PadKey::Confirm, false, false)]);
        // Each pad has its own stick.
        stick(&mut logic, 0, 1.0, 0.0);
        stick(&mut logic, 1, -1.0, 0.0);
        assert_eq!(
            flat(logic.update(now + ms(48), PadMode::Field)),
            [(PadKey::Left, true, false), (PadKey::Right, true, false)]
        );
    }

    #[test]
    fn a_pad_that_is_unplugged_lets_go_of_what_it_held() {
        let (mut logic, now) = (PadLogic::default(), t0());
        press(&mut logic, 3, Button::DPadRight);
        stick(&mut logic, 3, 0.0, 1.0);
        press(&mut logic, 3, Button::LeftTrigger2);
        assert_eq!(logic.update(now, PadMode::Field).len(), 3);
        logic.feed(3, RawInput::Gone);
        assert_eq!(
            flat(logic.update(now + ms(16), PadMode::Field)),
            [(PadKey::Up, false, false), (PadKey::Right, false, false), (PadKey::Walk, false, false)]
        );
        // Unplugging something that was never plugged in is harmless.
        logic.feed(9, RawInput::Gone);
        assert!(logic.update(now + ms(32), PadMode::Field).is_empty());
        // And it can come back and be used again.
        press(&mut logic, 3, Button::South);
        assert_eq!(flat(logic.update(now + ms(48), PadMode::Field)), [(PadKey::Confirm, true, false)]);
    }

    #[test]
    fn a_held_direction_repeats_in_menus_after_a_delay_and_then_steadily() {
        let (mut logic, now) = (PadLogic::default(), t0());
        press(&mut logic, 0, Button::DPadDown);
        assert_eq!(flat(logic.update(now, PadMode::Menu)), [(PadKey::Down, true, false)]);
        assert!(logic.update(now + REPEAT_DELAY - ms(1), PadMode::Menu).is_empty(), "not yet");
        assert_eq!(flat(logic.update(now + REPEAT_DELAY, PadMode::Menu)), [(PadKey::Down, true, true)]);
        assert!(logic.update(now + REPEAT_DELAY + REPEAT_INTERVAL - ms(1), PadMode::Menu).is_empty());
        assert_eq!(flat(logic.update(now + REPEAT_DELAY + REPEAT_INTERVAL, PadMode::Menu)), [(PadKey::Down, true, true)]);
        // A late frame repeats once, never a burst to catch up.
        assert_eq!(logic.update(now + ms(5000), PadMode::Menu).len(), 1);
        // Letting go ends it, and the next press starts the delay over.
        release(&mut logic, 0, Button::DPadDown);
        assert_eq!(flat(logic.update(now + ms(5016), PadMode::Menu)), [(PadKey::Down, false, false)]);
        press(&mut logic, 0, Button::DPadDown);
        assert_eq!(logic.update(now + ms(5032), PadMode::Menu).len(), 1);
        assert!(logic.update(now + ms(5032) + REPEAT_DELAY - ms(1), PadMode::Menu).is_empty());
    }

    #[test]
    fn nothing_repeats_in_the_field_and_no_other_button_repeats_anywhere() {
        let (mut logic, now) = (PadLogic::default(), t0());
        press(&mut logic, 0, Button::DPadUp);
        press(&mut logic, 0, Button::South);
        press(&mut logic, 0, Button::Start);
        assert_eq!(logic.update(now, PadMode::Field).len(), 3);
        for step in 1..=20 {
            assert!(logic.update(now + ms(100 * step), PadMode::Field).is_empty(), "step {step}");
        }
        // Into a menu (a battle began) with up still held: it repeats within a beat (the clock kept
        // running in the field, so at most one interval after the last frame), the others never do.
        let events = logic.update(now + ms(2000) + REPEAT_INTERVAL, PadMode::Menu);
        assert_eq!(flat(events), [(PadKey::Up, true, true)]);
        for step in 1..=20 {
            let events = logic.update(now + ms(2120 + 200 * step), PadMode::Menu);
            assert_eq!(flat(events), [(PadKey::Up, true, true)], "step {step}");
        }
    }

    #[test]
    fn a_stick_held_across_a_change_of_mode_keeps_one_direction_in_menus() {
        let (mut logic, now) = (PadLogic::default(), t0());
        stick(&mut logic, 0, 0.8, 0.75);
        assert_eq!(
            flat(logic.update(now, PadMode::Field)),
            [(PadKey::Up, true, false), (PadKey::Right, true, false)],
            "walking diagonally"
        );
        // A battle begins: the same push is now one direction, and the other is let go.
        let events = flat(logic.update(now + ms(16), PadMode::Menu));
        assert_eq!(events, [(PadKey::Up, false, false)], "the stronger axis (right) stays");
    }

    #[test]
    fn losing_the_focus_lets_go_and_what_is_still_held_when_it_returns_waits_for_a_fresh_press() {
        let (mut logic, now) = (PadLogic::default(), t0());
        press(&mut logic, 0, Button::DPadRight);
        press(&mut logic, 0, Button::South);
        assert_eq!(logic.update(now, PadMode::Field).len(), 2);
        logic.set_focused(false);
        assert_eq!(
            flat(logic.update(now + ms(16), PadMode::Field)),
            [(PadKey::Right, false, false), (PadKey::Confirm, false, false)]
        );
        // Pressing something while unfocused does nothing at all...
        press(&mut logic, 0, Button::North);
        assert!(logic.update(now + ms(32), PadMode::Field).is_empty());
        // ...and when the focus comes back everything still down is ignored until it is let go.
        logic.set_focused(true);
        assert!(logic.update(now + ms(48), PadMode::Field).is_empty(), "right, A and Y are all still down");
        release(&mut logic, 0, Button::South);
        assert!(logic.update(now + ms(64), PadMode::Field).is_empty());
        press(&mut logic, 0, Button::South);
        assert_eq!(flat(logic.update(now + ms(80), PadMode::Field)), [(PadKey::Confirm, true, false)], "a fresh press counts");
        // Y has to be seen let go before it counts again.
        release(&mut logic, 0, Button::North);
        assert!(logic.update(now + ms(96), PadMode::Field).is_empty());
        press(&mut logic, 0, Button::North);
        assert_eq!(flat(logic.update(now + ms(112), PadMode::Field)), [(PadKey::Accept, true, false)]);
        // Focus gained again without having been lost changes nothing.
        logic.set_focused(true);
        assert!(logic.update(now + ms(128), PadMode::Field).is_empty());
    }

    #[test]
    fn walking_is_a_held_trigger_and_the_right_bumper_asks_for_a_battle() {
        let (mut logic, now) = (PadLogic::default(), t0());
        press(&mut logic, 0, Button::LeftTrigger2);
        press(&mut logic, 0, Button::RightTrigger);
        assert_eq!(
            flat(logic.update(now, PadMode::Field)),
            [(PadKey::Battle, true, false), (PadKey::Walk, true, false)]
        );
        release(&mut logic, 0, Button::LeftTrigger2);
        assert_eq!(flat(logic.update(now + ms(16), PadMode::Field)), [(PadKey::Walk, false, false)]);
    }

    #[test]
    fn nonsense_axis_values_and_unused_axes_do_nothing() {
        let (mut logic, now) = (PadLogic::default(), t0());
        logic.feed(0, RawInput::Axis(Axis::LeftStickX, f32::NAN));
        logic.feed(0, RawInput::Axis(Axis::LeftStickY, f32::INFINITY));
        logic.feed(0, RawInput::Axis(Axis::RightStickX, 1.0));
        logic.feed(0, RawInput::Axis(Axis::DPadX, 1.0));
        logic.feed(0, RawInput::Axis(Axis::LeftZ, 1.0));
        logic.feed(0, RawInput::Axis(Axis::Unknown, 1.0));
        assert!(logic.update(now, PadMode::Field).is_empty());
        // A value beyond the range is clamped, not trusted.
        logic.feed(0, RawInput::Axis(Axis::LeftStickX, 7.5));
        assert_eq!(flat(logic.update(now + ms(16), PadMode::Field)), [(PadKey::Right, true, false)]);
    }

    /// By hand only (`cargo test -p fmv-client -- --ignored gamepad --nocapture`): opens the real
    /// backend (udev, `/dev/input`) and polls it for a moment. It proves the linking and the start-up
    /// on this machine, not any button: whatever is plugged in is idle during it.
    #[test]
    #[ignore = "opens the real gamepad backend (udev); run it by hand"]
    fn the_real_backend_opens_and_polls_without_trouble() {
        let mut pads = Pads::open();
        for _ in 0..30 {
            let events = pads.poll(Instant::now(), PadMode::Menu);
            eprintln!("poll: {events:?}");
            std::thread::sleep(ms(10));
        }
    }

    #[test]
    fn closed_pads_never_have_an_event() {
        let mut pads = Pads::closed();
        let now = t0();
        for mode in [PadMode::Field, PadMode::Menu] {
            assert!(pads.poll(now, mode).is_empty());
        }
        pads.set_focused(false);
        pads.set_focused(true);
        assert!(pads.poll(now + ms(10_000), PadMode::Menu).is_empty());
    }
}
