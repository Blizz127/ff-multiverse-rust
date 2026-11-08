//! Text: measuring, truncating, wrapping and drawing it onto a [`DrawList`]. Pure functions over
//! `&str` and plain data, no GPU.
//!
//! `DrawList::draw_text(x, y, scale, rgba, text)` is the entry point for drawing a string: one
//! textured rectangle per visible character, taken from the font atlas, at integer positions and
//! integer scales. [`DrawList::draw_text_shadowed`] adds a drop shadow.
//!
//! The font is monospace and every character takes one 8x8 cell ([`GLYPH_W`] pixels at scale 1),
//! so a line's width is its number of cells times the cell width. Control characters are never
//! drawn and take no space (text from a server is cleaned before it gets here, but a stray
//! newline must not break the layout). A character the font has no glyph for takes one cell and
//! is drawn as a hollow box, see [`crate::ui::font`].

#[cfg(test)]
use super::draw::DrawnText;
use super::draw::{DrawList, Quad, Rgba, MAX_SCALE};
use super::font::{cell_of, cell_uv, is_drawable, special_cell, Special, GLYPH_H, GLYPH_W};

/// Does `c` take space on a line? (Control characters do not.)
pub fn advances(c: char) -> bool {
    !c.is_control()
}

/// Number of cells `text` takes on one line.
pub fn cell_count(text: &str) -> usize {
    text.chars().filter(|c| advances(*c)).count()
}

/// Width of `text` in pixels at integer `scale` (a scale of 0 counts as 1).
pub fn text_width(text: &str, scale: u32) -> i32 {
    let scale = scale.max(1);
    i32::try_from(cell_count(text) as u64 * u64::from(GLYPH_W * scale)).unwrap_or(i32::MAX)
}

/// Height of one line of text in pixels at integer `scale`.
pub fn text_height(scale: u32) -> i32 {
    (GLYPH_H * scale.max(1)) as i32
}

/// How many characters of `text` the font can only draw as a box.
pub fn unsupported(text: &str) -> usize {
    text.chars().filter(|c| advances(*c) && !is_drawable(*c)).count()
}

/// `text` cut to at most `max` cells. A cut line ends in `...` when there is room for it (`max`
/// of 4 or more), else it is simply cut. Control characters are dropped.
pub fn ellipsize(text: &str, max: usize) -> String {
    let kept: Vec<char> = text.chars().filter(|c| advances(*c)).collect();
    if kept.len() <= max {
        return kept.into_iter().collect();
    }
    if max >= 4 {
        let mut s: String = kept[..max - 3].iter().collect();
        s.push_str("...");
        s
    } else {
        kept[..max].iter().collect()
    }
}

/// Greedy word wrap into lines of at most `max` cells (`max` of 0 counts as 1). Words are split
/// on spaces; a word longer than a line is cut into line-sized pieces; runs of spaces collapse to
/// one at a line break and are kept inside a line. A `\n` ends a paragraph (an empty paragraph is
/// an empty line). Empty text gives no lines.
pub fn wrap(text: &str, max: usize) -> Vec<String> {
    let max = max.max(1);
    if text.is_empty() {
        return Vec::new();
    }
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        let mut used = 0_usize;
        let flush = |line: &mut String, used: &mut usize, lines: &mut Vec<String>| {
            lines.push(std::mem::take(line));
            *used = 0;
        };
        for word in paragraph.split(' ').filter(|w| !w.is_empty()) {
            let chars: Vec<char> = word.chars().filter(|c| advances(*c)).collect();
            if chars.is_empty() {
                continue;
            }
            // A new word needs a separating space unless the line is empty.
            let needed = if used == 0 { chars.len() } else { chars.len() + 1 };
            if used + needed <= max {
                if used > 0 {
                    line.push(' ');
                    used += 1;
                }
                line.extend(chars.iter());
                used += chars.len();
                continue;
            }
            if used > 0 {
                flush(&mut line, &mut used, &mut lines);
            }
            // Pieces of a word that is longer than a whole line.
            let mut rest = &chars[..];
            while rest.len() > max {
                lines.push(rest[..max].iter().collect());
                rest = &rest[max..];
            }
            line.extend(rest.iter());
            used = rest.len();
        }
        lines.push(line);
    }
    lines
}

impl DrawList {
    /// Draw `text` with its top left corner at (`x`, `y`) in window pixels, every font pixel
    /// being `scale` x `scale` screen pixels (`scale` is clamped to `1..=MAX_SCALE`). Returns the
    /// width drawn in pixels. Control characters are skipped; a character the font lacks is drawn
    /// as a hollow box; spaces draw nothing.
    pub fn draw_text(&mut self, x: i32, y: i32, scale: u32, color: Rgba, text: &str) -> i32 {
        let scale = scale.clamp(1, MAX_SCALE);
        let cell = (GLYPH_W * scale) as i32;
        let mut cx = x;
        for c in text.chars().filter(|c| advances(*c)) {
            if c != ' ' && color[3] != 0 {
                self.quads.push(Quad { x: cx, y, w: cell, h: cell, uv: cell_uv(cell_of(c)), color });
            }
            cx = cx.saturating_add(cell);
        }
        #[cfg(test)]
        self.texts.push(DrawnText { x, y, scale, color, text: text.chars().filter(|c| advances(*c)).collect() });
        text_width(text, scale)
    }

    /// Like [`draw_text`](Self::draw_text) with a drop shadow (one font pixel down and to the
    /// right, in `shadow`) underneath, which keeps text readable over any background.
    pub fn draw_text_shadowed(&mut self, x: i32, y: i32, scale: u32, color: Rgba, shadow: Rgba, text: &str) -> i32 {
        let s = scale.clamp(1, MAX_SCALE) as i32;
        self.draw_text(x + s, y + s, scale, shadow, text);
        self.draw_text(x, y, scale, color, text)
    }

    /// One of the project's own glyphs (an arrow) at (`x`, `y`).
    pub fn draw_special(&mut self, x: i32, y: i32, scale: u32, color: Rgba, glyph: Special) {
        let scale = scale.clamp(1, MAX_SCALE);
        let cell = (GLYPH_W * scale) as i32;
        if color[3] != 0 {
            self.quads.push(Quad { x, y, w: cell, h: cell, uv: cell_uv(special_cell(glyph)), color });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths_follow_the_cell_count_and_the_scale() {
        assert_eq!(text_width("", 1), 0);
        assert_eq!(text_width("abc", 1), 24);
        assert_eq!(text_width("abc", 3), 72);
        assert_eq!(text_width("abc", 0), 24, "scale 0 counts as 1");
        assert_eq!(text_height(1), 8);
        assert_eq!(text_height(4), 32);
        // Control characters take no space; an unknown character takes one cell.
        assert_eq!(text_width("a\nb\u{1b}c", 1), 24);
        assert_eq!(text_width("a\u{e9}b", 1), 24);
        assert_eq!(unsupported("a\u{e9}b\u{4e2d}"), 2);
        assert_eq!(unsupported("plain ASCII ~!"), 0);
        assert_eq!(unsupported("\u{1b}\n"), 0, "control characters are not 'unsupported', they are skipped");
    }

    #[test]
    fn ellipsize_keeps_short_text_and_marks_a_cut() {
        assert_eq!(ellipsize("Cloud", 5), "Cloud");
        assert_eq!(ellipsize("Cloud", 10), "Cloud");
        assert_eq!(ellipsize("Cloud Strife", 8), "Cloud...");
        assert_eq!(ellipsize("Cloud Strife", 4), "C...");
        assert_eq!(ellipsize("Cloud Strife", 3), "Clo");
        assert_eq!(ellipsize("Cloud", 0), "");
        assert_eq!(cell_count(&ellipsize("a very long name indeed", 9)), 9);
        // Controls vanish before the length is counted.
        assert_eq!(ellipsize("a\nb\u{1b}c", 3), "abc");
        // Non-ASCII text is cut on characters, never inside one.
        assert_eq!(ellipsize("\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}", 5), "\u{e9}\u{e9}...");
    }

    #[test]
    fn wrap_breaks_on_spaces_and_never_exceeds_the_width() {
        assert_eq!(wrap("", 10), Vec::<String>::new());
        assert_eq!(wrap("hello world", 11), ["hello world"]);
        assert_eq!(wrap("hello world", 10), ["hello", "world"]);
        assert_eq!(wrap("one two three four", 9), ["one two", "three", "four"]);
        assert_eq!(wrap("a  b   c", 20), ["a b c"], "runs of spaces collapse");
        // A word longer than a line is cut into pieces.
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap("xx abcdefghij", 4), ["xx", "abcd", "efgh", "ij"]);
        // Paragraphs.
        assert_eq!(wrap("one\n\ntwo", 10), ["one", "", "two"]);
        // A width of 0 is treated as 1 instead of looping forever.
        assert_eq!(wrap("ab", 0), ["a", "b"]);
        for line in wrap("the quick brown fox jumps over the lazy dog and keeps running far away", 13) {
            assert!(cell_count(&line) <= 13, "{line:?}");
        }
    }

    #[test]
    fn wrapping_keeps_every_word_and_every_character() {
        let text = "that display name is already taken; pick another one";
        for width in [3_usize, 7, 10, 20, 80] {
            let joined = wrap(text, width).join(" ");
            let squeeze = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
            assert_eq!(squeeze(&joined), squeeze(text), "width {width}");
        }
    }
}
