//! Key events to menu input: the one place that knows winit's key types, and the one place a game
//! controller's buttons are turned into what a key press would have been.
//!
//! A menu wants two things from a key press: a navigation key ([`NavKey`], with its shift state and
//! whether it is the auto-repeat of a held key) or a piece of typed text. A press gives one or
//! the other, never both (Enter must not also type a carriage return).

use super::menu::{KeyPress, NavKey, PadScreen};
use crate::gamepad::PadKey;
use winit::keyboard::{Key, NamedKey};

/// What a key event means to a menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuInput {
    /// A navigation or editing key.
    Nav(KeyPress),
    /// Typed text (printable characters only).
    Text(String),
}

/// The text of a press, without control characters; `None` if nothing printable is left.
fn typed(text: &str) -> Option<MenuInput> {
    let printable: String = text.chars().filter(|c| !c.is_control()).collect();
    (!printable.is_empty()).then_some(MenuInput::Text(printable))
}

/// Translate one key event. `logical` is the key with modifiers applied, `text` what the press
/// typed (`KeyEvent::text`), `shift` whether Shift is held and `command` whether Ctrl, Alt or the
/// logo key is (those never type text: Ctrl+N is not an `n`). Releases mean nothing.
pub fn menu_input(logical: &Key, text: Option<&str>, pressed: bool, repeat: bool, shift: bool, command: bool) -> Option<MenuInput> {
    if !pressed {
        return None;
    }
    let nav = |key| Some(MenuInput::Nav(KeyPress { key, shift, repeat }));
    match logical {
        Key::Named(named) => match named {
            NamedKey::ArrowUp => nav(NavKey::Up),
            NamedKey::ArrowDown => nav(NavKey::Down),
            NamedKey::ArrowLeft => nav(NavKey::Left),
            NamedKey::ArrowRight => nav(NavKey::Right),
            NamedKey::Home => nav(NavKey::Home),
            NamedKey::End => nav(NavKey::End),
            NamedKey::PageUp => nav(NavKey::PageUp),
            NamedKey::PageDown => nav(NavKey::PageDown),
            NamedKey::Enter => nav(NavKey::Enter),
            NamedKey::Escape => nav(NavKey::Escape),
            NamedKey::Tab => nav(NavKey::Tab),
            NamedKey::Backspace => nav(NavKey::Backspace),
            NamedKey::Delete => nav(NavKey::Delete),
            NamedKey::Space if !command => typed(" "),
            _ => None,
        },
        Key::Character(s) if !command => typed(text.unwrap_or(s.as_str())),
        _ => None,
    }
}

/// What a controller button means to the character menu: the same [`MenuInput`] a key press makes,
/// so the menu has one way in. `screen` is where the button is heading ([`PadScreen`]), because a
/// few buttons mean different things on different screens and some must never act by accident:
///
/// | Button | List | Forget question | Create form | Connecting | Loading | Alert |
/// |---|---|---|---|---|---|---|
/// | stick, D-pad | Up, Down | switch answer | move, change a value | - | - | - |
/// | A | Enter: play | Enter: the chosen answer | Enter: create (or Back) | - | - | Enter: dismiss |
/// | B | - | Esc: keep | Esc: back (only if there is a list) | Esc: cancel | - | - |
/// | Start | Esc: quit | Esc: keep | Esc: back, or quit | Esc: cancel | Esc: quit | - |
/// | X | `n`: new character | `n`: keep | Backspace | - | - | - |
/// | Y | Del: asks to forget | - | - | - | - | - |
///
/// B never quits the client (it is "back": where there is nothing to go back to it does nothing),
/// Start is the Esc key and the only way out, a notice that holds a secret is dismissed by A alone,
/// and no controller button ever types a letter into the name field. Y does not answer "yes" to the
/// forget question, so pressing it twice cannot forget a login. Releases mean nothing.
pub fn pad_menu_input(key: PadKey, pressed: bool, repeat: bool, screen: PadScreen) -> Option<MenuInput> {
    if !pressed {
        return None;
    }
    let nav = |key| Some(MenuInput::Nav(KeyPress { key, shift: false, repeat }));
    let text = |text: &str| Some(MenuInput::Text(text.to_string()));
    match key {
        PadKey::Up => nav(NavKey::Up),
        PadKey::Down => nav(NavKey::Down),
        PadKey::Left => nav(NavKey::Left),
        PadKey::Right => nav(NavKey::Right),
        PadKey::Confirm => nav(NavKey::Enter),
        PadKey::Back => match screen {
            PadScreen::Question | PadScreen::Connecting | PadScreen::Form { can_go_back: true } => nav(NavKey::Escape),
            PadScreen::List | PadScreen::Form { can_go_back: false } | PadScreen::Loading | PadScreen::Alert => None,
        },
        PadKey::Escape => match screen {
            PadScreen::Alert => None,
            _ => nav(NavKey::Escape),
        },
        PadKey::Invite => match screen {
            PadScreen::List | PadScreen::Question => text("n"),
            PadScreen::Form { .. } => nav(NavKey::Backspace),
            PadScreen::Connecting | PadScreen::Loading | PadScreen::Alert => None,
        },
        PadKey::Accept => match screen {
            PadScreen::List => nav(NavKey::Delete),
            _ => None,
        },
        PadKey::Leave | PadKey::Battle | PadKey::Walk => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(key: Key) -> Option<MenuInput> {
        menu_input(&key, None, true, false, false, false)
    }

    fn nav(key: NavKey) -> Option<MenuInput> {
        Some(MenuInput::Nav(KeyPress::new(key)))
    }

    #[test]
    fn navigation_keys_map_one_to_one() {
        for (named, want) in [
            (NamedKey::ArrowUp, NavKey::Up),
            (NamedKey::ArrowDown, NavKey::Down),
            (NamedKey::ArrowLeft, NavKey::Left),
            (NamedKey::ArrowRight, NavKey::Right),
            (NamedKey::Home, NavKey::Home),
            (NamedKey::End, NavKey::End),
            (NamedKey::PageUp, NavKey::PageUp),
            (NamedKey::PageDown, NavKey::PageDown),
            (NamedKey::Enter, NavKey::Enter),
            (NamedKey::Escape, NavKey::Escape),
            (NamedKey::Tab, NavKey::Tab),
            (NamedKey::Backspace, NavKey::Backspace),
            (NamedKey::Delete, NavKey::Delete),
        ] {
            assert_eq!(press(Key::Named(named)), nav(want), "{named:?}");
        }
    }

    #[test]
    fn a_navigation_key_carries_shift_and_repeat() {
        let k = Key::Named(NamedKey::Tab);
        assert_eq!(
            menu_input(&k, Some("\t"), true, false, true, false),
            Some(MenuInput::Nav(KeyPress { key: NavKey::Tab, shift: true, repeat: false }))
        );
        assert_eq!(
            menu_input(&Key::Named(NamedKey::ArrowLeft), None, true, true, false, false),
            Some(MenuInput::Nav(KeyPress { key: NavKey::Left, shift: false, repeat: true }))
        );
    }

    #[test]
    fn enter_and_friends_never_also_type_their_control_character() {
        // winit reports Enter with text "\r", Backspace with "\u{8}", Delete with "\u{7f}"...
        for (named, text) in [(NamedKey::Enter, "\r"), (NamedKey::Backspace, "\u{8}"), (NamedKey::Delete, "\u{7f}"), (NamedKey::Tab, "\t"), (NamedKey::Escape, "\u{1b}")] {
            let got = menu_input(&Key::Named(named), Some(text), true, false, false, false);
            assert!(matches!(got, Some(MenuInput::Nav(_))), "{named:?} -> {got:?}");
        }
    }

    #[test]
    fn letters_and_the_space_bar_type_text() {
        assert_eq!(press(Key::Character("a".into())), Some(MenuInput::Text("a".into())));
        assert_eq!(press(Key::Named(NamedKey::Space)), Some(MenuInput::Text(" ".into())));
        // What the press typed (shift and composition applied) wins over the key's own name.
        assert_eq!(
            menu_input(&Key::Character("a".into()), Some("A"), true, false, true, false),
            Some(MenuInput::Text("A".into()))
        );
        assert_eq!(
            menu_input(&Key::Character("e".into()), Some("\u{e9}"), true, false, false, false),
            Some(MenuInput::Text("\u{e9}".into()))
        );
        // A held letter keeps typing.
        assert_eq!(
            menu_input(&Key::Character("x".into()), Some("x"), true, true, false, false),
            Some(MenuInput::Text("x".into()))
        );
    }

    #[test]
    fn control_characters_are_not_text() {
        assert_eq!(menu_input(&Key::Character("c".into()), Some("\u{3}"), true, false, false, false), None);
        assert_eq!(typed("\u{1b}\n"), None);
        assert_eq!(typed("a\u{1b}b"), Some(MenuInput::Text("ab".into())));
    }

    #[test]
    fn a_shortcut_chord_types_nothing() {
        // Ctrl+N is not an `n`, Alt+Space is not a space.
        assert_eq!(menu_input(&Key::Character("n".into()), Some("n"), true, false, false, true), None);
        assert_eq!(menu_input(&Key::Named(NamedKey::Space), None, true, false, false, true), None);
        // ...but Ctrl+Left is still a Left.
        assert_eq!(
            menu_input(&Key::Named(NamedKey::ArrowLeft), None, true, false, false, true),
            nav(NavKey::Left)
        );
    }

    #[test]
    fn releases_and_unrelated_keys_mean_nothing() {
        assert_eq!(menu_input(&Key::Named(NamedKey::Enter), None, false, false, false, false), None);
        assert_eq!(menu_input(&Key::Character("a".into()), Some("a"), false, false, false, false), None);
        for named in [NamedKey::F1, NamedKey::Shift, NamedKey::Control, NamedKey::CapsLock, NamedKey::Insert] {
            assert_eq!(press(Key::Named(named)), None, "{named:?}");
        }
        assert_eq!(press(Key::Unidentified(winit::keyboard::NativeKey::Unidentified)), None);
    }

    // ---- the controller ------------------------------------------------------------------------

    use crate::ui::menu::{CharacterEntry, Menu, MenuAction, MenuConfig, Notice, Screen};
    use fmv_core::GameId;

    const SCREENS: [PadScreen; 7] = [
        PadScreen::List,
        PadScreen::Question,
        PadScreen::Form { can_go_back: true },
        PadScreen::Form { can_go_back: false },
        PadScreen::Connecting,
        PadScreen::Loading,
        PadScreen::Alert,
    ];

    fn mapped(key: PadKey, screen: PadScreen) -> Option<MenuInput> {
        pad_menu_input(key, true, false, screen)
    }

    fn typed_text(text: &str) -> Option<MenuInput> {
        Some(MenuInput::Text(text.to_string()))
    }

    #[test]
    fn the_buttons_mean_what_the_table_says_on_every_screen() {
        for screen in SCREENS {
            for (key, want) in [
                (PadKey::Up, NavKey::Up),
                (PadKey::Down, NavKey::Down),
                (PadKey::Left, NavKey::Left),
                (PadKey::Right, NavKey::Right),
                (PadKey::Confirm, NavKey::Enter),
            ] {
                assert_eq!(mapped(key, screen), nav(want), "{key:?} on {screen:?}");
            }
            for key in [PadKey::Leave, PadKey::Battle, PadKey::Walk] {
                assert_eq!(mapped(key, screen), None, "{key:?} does nothing in the menu ({screen:?})");
            }
            for key in PadKey::ALL {
                assert_eq!(pad_menu_input(key, false, false, screen), None, "a release of {key:?} means nothing");
            }
            // Start is Esc everywhere, except over a notice that must be read.
            let start = if screen == PadScreen::Alert { None } else { nav(NavKey::Escape) };
            assert_eq!(mapped(PadKey::Escape, screen), start, "Start on {screen:?}");
        }
        for (screen, want) in [
            (PadScreen::List, None),
            (PadScreen::Question, nav(NavKey::Escape)),
            (PadScreen::Form { can_go_back: true }, nav(NavKey::Escape)),
            (PadScreen::Form { can_go_back: false }, None),
            (PadScreen::Connecting, nav(NavKey::Escape)),
            (PadScreen::Loading, None),
            (PadScreen::Alert, None),
        ] {
            assert_eq!(mapped(PadKey::Back, screen), want, "B on {screen:?}");
        }
        for (screen, want) in [
            (PadScreen::List, typed_text("n")),
            (PadScreen::Question, typed_text("n")),
            (PadScreen::Form { can_go_back: true }, nav(NavKey::Backspace)),
            (PadScreen::Form { can_go_back: false }, nav(NavKey::Backspace)),
            (PadScreen::Connecting, None),
            (PadScreen::Loading, None),
            (PadScreen::Alert, None),
        ] {
            assert_eq!(mapped(PadKey::Invite, screen), want, "X on {screen:?}");
        }
        for screen in SCREENS {
            let want = if screen == PadScreen::List { nav(NavKey::Delete) } else { None };
            assert_eq!(mapped(PadKey::Accept, screen), want, "Y on {screen:?}");
        }
    }

    #[test]
    fn a_held_direction_repeats_into_the_menu_and_nothing_else_does() {
        assert_eq!(
            pad_menu_input(PadKey::Down, true, true, PadScreen::List),
            Some(MenuInput::Nav(KeyPress { key: NavKey::Down, shift: false, repeat: true }))
        );
        assert_eq!(pad_menu_input(PadKey::Confirm, true, false, PadScreen::List), nav(NavKey::Enter));
    }

    fn menu_with(names: &[&str]) -> Menu {
        let entries = names.iter().map(|n| CharacterEntry { name: (*n).to_string(), look: None, loose_permissions: false }).collect();
        Menu::new(MenuConfig::new("127.0.0.1:47770", GameId::Ff7), entries)
    }

    /// A button pressed on a menu the way the window does it: through the mapping into the menu.
    fn pad(menu: &mut Menu, key: PadKey) -> Option<MenuAction> {
        match pad_menu_input(key, true, false, menu.pad_screen()) {
            Some(MenuInput::Nav(press)) => menu.key(press),
            Some(MenuInput::Text(text)) => menu.text(&text),
            None => None,
        }
    }

    fn name_typed(menu: &Menu) -> String {
        match menu.screen() {
            Screen::Create(form) => form.name.text(),
            other => panic!("not the create form: {other:?}"),
        }
    }

    /// A controller alone can choose a character, create one, back out of things and quit.
    #[test]
    fn a_controller_alone_gets_through_the_menu() {
        let mut m = menu_with(&["Cloud", "Tifa"]);
        assert_eq!(pad(&mut m, PadKey::Down), None);
        assert_eq!(m.selected_name(), Some("Tifa"));
        assert_eq!(pad(&mut m, PadKey::Confirm), Some(MenuAction::Play { index: 1 }));
        assert!(m.is_connecting());
        assert_eq!(pad(&mut m, PadKey::Back), Some(MenuAction::CancelConnect), "B cancels the attempt");
        assert_eq!(m.selected_name(), Some("Tifa"));
        // X: a new character; B: back to the list.
        assert_eq!(pad(&mut m, PadKey::Invite), None);
        assert!(matches!(m.screen(), Screen::Create(_)));
        assert_eq!(pad(&mut m, PadKey::Back), None);
        assert!(matches!(m.screen(), Screen::Select(_)));
        // B does not quit from the list; Start does.
        assert_eq!(pad(&mut m, PadKey::Back), None);
        assert!(matches!(m.screen(), Screen::Select(_)));
        assert_eq!(pad(&mut m, PadKey::Escape), Some(MenuAction::Quit));
        // Create form: the stick moves between the fields and changes values, A creates.
        assert_eq!(pad(&mut m, PadKey::Invite), None);
        assert_eq!(pad(&mut m, PadKey::Down), None);
        assert_eq!(pad(&mut m, PadKey::Right), None);
        match m.screen() {
            Screen::Create(f) => {
                let default = crate::ui::menu::body_options(GameId::Ff7).iter().position(|o| o.id == fmv_ff7::playable::DEFAULT_BODY).unwrap_or(0);
                let next = (default + 1) % crate::ui::menu::body_options(GameId::Ff7).len();
                assert_eq!((f.focus, f.body), (crate::ui::menu::Field::Body, next), "down to the body, right picks the next one")
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(pad(&mut m, PadKey::Confirm), None, "no name yet: refused with a notice, nothing is sent");
        assert!(m.notice().is_some());
    }

    #[test]
    fn y_asks_to_forget_and_pressing_it_again_cannot_forget() {
        let mut m = menu_with(&["Cloud", "Tifa"]);
        assert_eq!(pad(&mut m, PadKey::Accept), None);
        assert_eq!(m.pad_screen(), PadScreen::Question);
        // A second Y (a double press) does nothing: the question stays, nothing is forgotten.
        assert_eq!(pad(&mut m, PadKey::Accept), None);
        assert_eq!(m.pad_screen(), PadScreen::Question);
        // A on the default answer keeps the login...
        assert_eq!(pad(&mut m, PadKey::Confirm), None);
        assert_eq!(m.pad_screen(), PadScreen::List);
        // ...B and X also say no...
        for key in [PadKey::Back, PadKey::Invite] {
            pad(&mut m, PadKey::Accept);
            assert_eq!(pad(&mut m, key), None, "{key:?}");
            assert_eq!(m.pad_screen(), PadScreen::List, "{key:?} keeps the login");
        }
        // ...and forgetting takes a deliberate move to the other answer and A.
        pad(&mut m, PadKey::Accept);
        assert_eq!(pad(&mut m, PadKey::Right), None);
        assert_eq!(pad(&mut m, PadKey::Confirm), Some(MenuAction::Forget { index: 0 }));
    }

    /// No controller button types a letter into the name field, and none of them but Start leaves.
    #[test]
    fn no_button_types_into_the_name_field_or_quits_by_accident() {
        let mut m = menu_with(&[]);
        assert!(m.text_input_focused());
        for key in PadKey::ALL {
            if key == PadKey::Escape {
                continue;
            }
            let action = pad(&mut m, key);
            assert!(!matches!(action, Some(MenuAction::Quit | MenuAction::Register { .. })), "{key:?} -> {action:?}");
            assert_eq!(name_typed(&m), "", "{key:?} typed something");
        }
        assert!(matches!(m.screen(), Screen::Create(_)), "still on the form: B did not leave it");
        // With a name typed, X deletes the last letter and Y does nothing.
        let mut m = menu_with(&[]);
        assert_eq!(m.text("Cloud"), None);
        assert_eq!(pad(&mut m, PadKey::Invite), None);
        assert_eq!(name_typed(&m), "Clou");
        assert_eq!(pad(&mut m, PadKey::Accept), None);
        assert_eq!(name_typed(&m), "Clou");
        // Start is the way out.
        assert_eq!(pad(&mut m, PadKey::Escape), Some(MenuAction::Quit));
    }

    /// A notice that holds a token the player must read is dismissed by A and nothing else.
    #[test]
    fn a_secret_on_screen_is_dismissed_by_a_alone() {
        let mut m = menu_with(&["Cloud"]);
        m.set_notice(Notice::error("the login could not be saved").with_secret("fmvt_abc"));
        for key in [PadKey::Back, PadKey::Escape, PadKey::Invite, PadKey::Accept, PadKey::Up, PadKey::Down, PadKey::Left, PadKey::Right] {
            assert_eq!(pad(&mut m, key), None, "{key:?}");
            assert!(m.notice().is_some(), "{key:?} must not dismiss it");
        }
        assert_eq!(pad(&mut m, PadKey::Confirm), None);
        assert!(m.notice().is_none(), "A dismisses it");
    }
}
