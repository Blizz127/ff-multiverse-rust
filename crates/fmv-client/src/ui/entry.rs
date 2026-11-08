//! Editing one line of text: the character name field. Pure: key events in, text and cursor out.
//!
//! What goes into the field is already what the server will accept: control characters and
//! invisible formatting characters (zero-width joiners, bidi overrides, ...) are refused when
//! typed, and at most [`AvatarAppearance::MAX_NAME`] characters fit. The name that is finally
//! registered is [`TextEntry::name`], the same `sanitize_name` the server applies.

use fmv_core::{is_invisible_format, AvatarAppearance};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEntry {
    chars: Vec<char>,
    /// The insertion point, `0..=len`: the new character goes before the one at this index.
    cursor: usize,
    max: usize,
}

impl TextEntry {
    /// An empty field that holds at most `max` characters.
    pub fn new(max: usize) -> Self {
        Self { chars: Vec::new(), cursor: 0, max }
    }

    /// An empty field for a display name (24 characters, the server's limit).
    pub fn for_names() -> Self {
        Self::new(AvatarAppearance::MAX_NAME)
    }

    /// Would the field take `c`? Not a control character, not an invisible formatting one.
    pub fn accepts(c: char) -> bool {
        !c.is_control() && !is_invisible_format(c)
    }

    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    pub fn len(&self) -> usize {
        self.chars.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    pub fn max(&self) -> usize {
        self.max
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The name this text stands for after the server's own cleaning (`None`: nothing visible).
    pub fn name(&self) -> Option<String> {
        AvatarAppearance::sanitize_name(&self.text())
    }

    /// Insert `c` at the cursor. `false` when it is refused (see [`accepts`](Self::accepts)) or
    /// the field is full.
    pub fn insert(&mut self, c: char) -> bool {
        if !Self::accepts(c) || self.chars.len() >= self.max {
            return false;
        }
        self.chars.insert(self.cursor, c);
        self.cursor += 1;
        true
    }

    /// Insert as much of `s` as fits and is accepted; how many characters went in.
    #[cfg(test)]
    pub fn insert_str(&mut self, s: &str) -> usize {
        s.chars().filter(|c| self.insert(*c)).count()
    }

    /// Delete the character before the cursor.
    pub fn backspace(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        self.cursor -= 1;
        self.chars.remove(self.cursor);
        true
    }

    /// Delete the character at the cursor.
    pub fn delete(&mut self) -> bool {
        if self.cursor >= self.chars.len() {
            return false;
        }
        self.chars.remove(self.cursor);
        true
    }

    pub fn left(&mut self) -> bool {
        let moved = self.cursor > 0;
        self.cursor = self.cursor.saturating_sub(1);
        moved
    }

    pub fn right(&mut self) -> bool {
        let moved = self.cursor < self.chars.len();
        self.cursor = (self.cursor + 1).min(self.chars.len());
        moved
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.chars.len();
    }

    #[cfg(test)]
    pub fn clear(&mut self) {
        self.chars.clear();
        self.cursor = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(s: &str) -> TextEntry {
        let mut e = TextEntry::for_names();
        e.insert_str(s);
        e
    }

    #[test]
    fn typing_appends_at_the_cursor_and_the_cursor_follows() {
        let mut e = TextEntry::for_names();
        assert!(e.is_empty() && e.cursor() == 0 && e.max() == 24);
        for c in "Cloud".chars() {
            assert!(e.insert(c));
        }
        assert_eq!((e.text().as_str(), e.cursor(), e.len()), ("Cloud", 5, 5));
        // In the middle: the new character goes before the one the cursor is on.
        e.left();
        e.left();
        assert!(e.insert('X'));
        assert_eq!((e.text().as_str(), e.cursor()), ("CloXud", 4));
        e.home();
        assert!(e.insert('>'));
        assert_eq!((e.text().as_str(), e.cursor()), (">CloXud", 1));
        e.end();
        assert!(e.insert('!'));
        assert_eq!((e.text().as_str(), e.cursor()), (">CloXud!", 8));
    }

    #[test]
    fn backspace_and_delete_remove_the_right_neighbour_and_stop_at_the_ends() {
        let mut e = typed("abcd");
        assert!(e.backspace());
        assert_eq!((e.text().as_str(), e.cursor()), ("abc", 3));
        assert!(!e.delete(), "nothing after the cursor");
        e.left();
        assert!(e.delete());
        assert_eq!((e.text().as_str(), e.cursor()), ("ab", 2));
        e.home();
        assert!(!e.backspace(), "nothing before the cursor");
        assert!(e.delete());
        assert_eq!((e.text().as_str(), e.cursor()), ("b", 0));
        assert!(e.delete() && !e.delete() && e.is_empty());
        assert!(!e.backspace());
    }

    #[test]
    fn the_cursor_is_clamped_and_reports_whether_it_moved() {
        let mut e = typed("ab");
        assert!(!e.right(), "already at the end");
        assert!(e.left() && e.left());
        assert!(!e.left(), "already at the start");
        assert_eq!(e.cursor(), 0);
        assert!(e.right());
        assert_eq!(e.cursor(), 1);
        e.end();
        assert_eq!(e.cursor(), 2);
        e.clear();
        assert!(e.is_empty() && e.cursor() == 0);
    }

    #[test]
    fn at_most_twenty_four_characters_fit() {
        let mut e = TextEntry::for_names();
        let n = e.insert_str(&"x".repeat(100));
        assert_eq!(n, 24);
        assert_eq!(e.len(), 24);
        assert!(!e.insert('y'), "full");
        // Making room lets the next one in.
        e.backspace();
        assert!(e.insert('y'));
        assert_eq!(e.text(), format!("{}y", "x".repeat(23)));
        // The 24 characters are exactly what the server keeps.
        assert_eq!(e.name().unwrap().chars().count(), 24);
    }

    #[test]
    fn control_and_invisible_characters_are_refused_when_typed() {
        let mut e = TextEntry::for_names();
        for c in ['\n', '\r', '\t', '\u{1b}', '\u{7f}', '\u{0}', '\u{8}', '\u{200b}', '\u{202e}', '\u{feff}', '\u{2028}'] {
            assert!(!e.insert(c), "{c:?}");
            assert!(!TextEntry::accepts(c), "{c:?}");
        }
        assert!(e.is_empty());
        // Everything else a keyboard produces is fine: letters of any script, digits, spaces.
        for c in ['a', 'Z', '7', ' ', '-', '_', '\u{e9}', '\u{4e2d}', '\u{1f600}'] {
            assert!(TextEntry::accepts(c), "{c:?}");
        }
        assert_eq!(e.insert_str("Cl\u{200b}ou\u{1b}d\n"), 5);
        assert_eq!(e.text(), "Cloud");
    }

    #[test]
    fn the_registered_name_is_the_servers_sanitised_name() {
        assert_eq!(typed("  Tifa  ").name().as_deref(), Some("Tifa"), "surrounding spaces are trimmed");
        assert_eq!(typed("Red XIII").name().as_deref(), Some("Red XIII"), "inner spaces stay");
        assert_eq!(typed("").name(), None);
        assert_eq!(typed("    ").name(), None, "nothing visible is no name");
        // The same function the CLI and the server use.
        for raw in ["Barret", " Aerith ", "a  b"] {
            assert_eq!(typed(raw).name(), AvatarAppearance::sanitize_name(raw), "{raw:?}");
        }
    }

    #[test]
    fn editing_inside_multibyte_text_never_splits_a_character() {
        let mut e = typed("h\u{e9}llo \u{4e2d}\u{6587}");
        assert_eq!(e.len(), 8);
        e.end();
        e.backspace();
        assert_eq!(e.text(), "h\u{e9}llo \u{4e2d}");
        e.home();
        e.right();
        e.delete();
        assert_eq!(e.text(), "hllo \u{4e2d}");
        assert!(e.insert('\u{e8}'));
        assert_eq!(e.text(), "h\u{e8}llo \u{4e2d}");
    }
}
