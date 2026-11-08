//! Keyboard events to menu input: the one place that knows winit's key types.
//!
//! A menu wants two things from a key press: a navigation key ([`NavKey`], with its shift state and
//! whether it is the auto-repeat of a held key) or a piece of typed text. A press gives one or
//! the other, never both (Enter must not also type a carriage return).

use super::menu::{KeyPress, NavKey};
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
}
