//! Turning a message into pages of rows of coloured text that fit a dialogue window: folding the
//! text to what the client's 8x8 font can draw, word wrapping in colour runs, pages, and where the
//! rows of a question's choices go. Pure functions over plain data; no GPU.
//!
//! # The rules
//!
//! * **Pages.** A message is split at its page breaks first (the game's `NEWPAGE`); every page is
//!   then cut into pages of at most `max_rows` rows, so nothing is ever clipped by a window that is
//!   smaller than the one the game gave the text.
//! * **Lines first, then wrapping.** The line breaks of the text are the game's own. A line is only
//!   wrapped when it is wider than the window, at spaces, keeping the colour of every character and
//!   the indent the line started with.
//! * **Choices.** An `ASK` numbers its choice lines on the **last page** of the message, from 0, in
//!   *hard* lines (before any wrapping, and whatever markers the text has). Choice rows are never
//!   wrapped (a cut one ends in `...`), lose their leading blanks (the game's tabs and cursor
//!   column; the window draws its own pointer), and stay together on one page. A list that does not
//!   fit the window scrolls.
//! * **Folding.** The font has the printable ASCII only. The typographic quotes and the ellipsis of
//!   the Steam data and the accented letters of the game's text table become their ASCII
//!   look-alikes; anything else becomes `?`, never a box.

use crate::ui::font::is_drawable;
use crate::ui::text::cell_count;
use fmv_ff7::script::dialogue::{Choice, Message, Piece, TextColour};
use std::ops::Range;

/// Cells (8 canvas units each) the pointer column of a choice row takes.
pub const POINTER_CELLS: usize = 2;

/// What the 8x8 font draws for one character: the character itself when it is printable ASCII, an
/// ASCII stand-in for the marks and letters of the game's text table, `?` for the rest. Nothing for
/// control characters.
pub fn fold_char(c: char, out: &mut String) {
    if is_drawable(c) {
        out.push(c);
        return;
    }
    let s = match c {
        c if c.is_control() => return,
        '\u{201c}' | '\u{201d}' | '\u{201e}' | '\u{ab}' | '\u{bb}' => "\"",
        '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{b4}' | '\u{2039}' | '\u{203a}' => "'",
        '\u{2026}' => "...",
        '\u{2013}' | '\u{2014}' => "-",
        '\u{a0}' => " ",
        'À' | 'Á' | 'Â' | 'Ã' | 'Ä' | 'Å' => "A",
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => "a",
        'Ç' => "C",
        'ç' => "c",
        'È' | 'É' | 'Ê' | 'Ë' => "E",
        'è' | 'é' | 'ê' | 'ë' => "e",
        'Ì' | 'Í' | 'Î' | 'Ï' => "I",
        'ì' | 'í' | 'î' | 'ï' => "i",
        'Ñ' => "N",
        'ñ' => "n",
        'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ö' | 'Ø' => "O",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' => "o",
        'Ù' | 'Ú' | 'Û' | 'Ü' => "U",
        'ù' | 'ú' | 'û' | 'ü' => "u",
        'Ý' | 'Ÿ' => "Y",
        'ý' | 'ÿ' => "y",
        'ß' => "ss",
        'Æ' => "AE",
        'æ' => "ae",
        'Œ' => "OE",
        'œ' => "oe",
        '¿' => "?",
        '¡' => "!",
        '©' => "(c)",
        '®' => "(R)",
        '™' => "TM",
        '°' => "o",
        '±' => "+/-",
        '÷' => "/",
        '×' => "x",
        'µ' => "u",
        '¢' => "c",
        '£' => "L",
        '¥' => "Y",
        '¤' | '■' | '▪' | '◊' => "*",
        '≠' => "!=",
        '≤' => "<=",
        '≥' => ">=",
        '≈' => "~",
        _ => "?",
    };
    out.push_str(s);
}

/// [`fold_char`] over a string.
pub fn fold(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        fold_char(c, &mut out);
    }
    out
}

/// A stretch of text in one colour (ASCII only: it is folded).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub text: String,
    pub colour: TextColour,
}

/// One row of a window: runs left to right.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Row {
    pub runs: Vec<Run>,
}

impl Row {
    /// Cells the row takes.
    pub fn cells(&self) -> usize {
        self.runs.iter().map(|r| cell_count(&r.text)).sum()
    }

    /// The row as plain text.
    #[cfg(test)]
    pub fn plain(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect()
    }

    fn from_cells(cells: &[(char, TextColour)]) -> Self {
        let mut runs: Vec<Run> = Vec::new();
        for &(c, colour) in cells {
            match runs.last_mut() {
                Some(run) if run.colour == colour => run.text.push(c),
                _ => runs.push(Run { text: c.to_string(), colour }),
            }
        }
        Self { runs }
    }
}

/// One page of a message as the window shows it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Page {
    pub rows: Vec<Row>,
    /// The rows of `rows` that are the choices, in order (the pointer is on one of them).
    pub choices: Option<Range<usize>>,
}

/// A message laid out for a window `cells` wide (inside its padding) and `rows` rows tall.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Laid {
    pub pages: Vec<Page>,
    /// Width of the widest row, in cells (a choice row counts its pointer column).
    pub cells: usize,
    /// Rows the window shows at a time: the tallest page, at most the limit asked for.
    pub rows: usize,
}

/// A line of the text before wrapping.
#[derive(Debug, Clone, Default)]
struct HardLine {
    cells: Vec<(char, TextColour)>,
}

/// The lines of each page of `pieces` (a page break starts a new list).
fn hard_pages(pieces: &[Piece]) -> Vec<Vec<HardLine>> {
    let mut pages = vec![vec![HardLine::default()]];
    let mut colour = TextColour::White;
    for piece in pieces {
        match piece {
            Piece::Text(t) => {
                if let Some(line) = pages.last_mut().and_then(|p| p.last_mut()) {
                    line.cells.extend(fold(t).chars().map(|c| (c, colour)));
                }
            }
            Piece::NewLine => {
                if let Some(page) = pages.last_mut() {
                    page.push(HardLine::default());
                }
            }
            Piece::NewPage => pages.push(vec![HardLine::default()]),
            Piece::Colour(c) => colour = *c,
            // The cursor column: the window draws its own pointer.
            Piece::Choice => {}
        }
    }
    pages
}

/// Wrap one hard line to rows of at most `max` cells (at least 1): at spaces, a word longer than a
/// row cut in pieces, the indent of the line kept on every row (as long as it leaves room).
fn wrap_line(line: &HardLine, max: usize) -> Vec<Row> {
    let max = max.max(1);
    let indent = line.cells.iter().take_while(|(c, _)| *c == ' ').count();
    let indent = if indent * 2 <= max { indent } else { 0 };
    let body = line.cells.get(indent..).unwrap_or(&[]);
    let lead: Vec<(char, TextColour)> = vec![(' ', TextColour::White); indent];
    let room = max.saturating_sub(indent).max(1);

    // Words and the blanks between them.
    let mut words: Vec<(Vec<(char, TextColour)>, usize)> = Vec::new(); // (word, blanks before it)
    let (mut blanks, mut word): (usize, Vec<(char, TextColour)>) = (0, Vec::new());
    for &(c, colour) in body {
        if c == ' ' {
            if !word.is_empty() {
                words.push((std::mem::take(&mut word), blanks));
                blanks = 0;
            }
            blanks += 1;
        } else {
            word.push((c, colour));
        }
    }
    if !word.is_empty() {
        words.push((word, blanks));
    }

    let mut rows: Vec<Vec<(char, TextColour)>> = Vec::new();
    let mut cur: Vec<(char, TextColour)> = Vec::new();
    for (word, before) in words {
        let gap = if cur.is_empty() { 0 } else { before };
        if !cur.is_empty() && cur.len() + gap + word.len() <= room {
            cur.extend(std::iter::repeat_n((' ', TextColour::White), gap));
            cur.extend(word);
            continue;
        }
        if !cur.is_empty() {
            rows.push(std::mem::take(&mut cur));
        }
        // A word wider than a row is cut into row-sized pieces.
        let mut rest: &[(char, TextColour)] = &word;
        while rest.len() > room {
            let (head, tail) = rest.split_at(room);
            rows.push(head.to_vec());
            rest = tail;
        }
        cur = rest.to_vec();
    }
    if !cur.is_empty() || rows.is_empty() {
        rows.push(cur);
    }
    rows.into_iter()
        .map(|cells| {
            let mut all = lead.clone();
            all.extend(cells);
            // A row that is only indent is an empty row.
            if all.iter().all(|(c, _)| *c == ' ') {
                all.clear();
            }
            Row::from_cells(&all)
        })
        .collect()
}

/// A choice row: no leading blanks, cut with `...` when wider than `max` cells.
fn choice_row(line: &HardLine, max: usize) -> Row {
    let cells: Vec<(char, TextColour)> = line.cells.iter().copied().skip_while(|(c, _)| *c == ' ').collect();
    let max = max.max(1);
    if cells.len() <= max {
        return Row::from_cells(&cells);
    }
    let mut cut: Vec<(char, TextColour)> = cells.iter().copied().take(if max >= 4 { max - 3 } else { max }).collect();
    if max >= 4 {
        let colour = cells.get(max - 4).map_or(TextColour::White, |c| c.1);
        cut.extend(std::iter::repeat_n(('.', colour), 3));
    }
    Row::from_cells(&cut)
}

/// Whether the message shows no character at all (a window of nothing but codes).
pub fn is_blank(message: &Message) -> bool {
    message.choice.is_none()
        && message.pieces.iter().all(|p| match p {
            Piece::Text(t) => fold(t).trim().is_empty(),
            _ => true,
        })
}

/// Lay `message` out for a window of at most `max_cells` cells inside its padding and `max_rows`
/// rows (both at least 1 and 2).
pub fn lay_out(message: &Message, max_cells: usize, max_rows: usize) -> Laid {
    let max_cells = max_cells.max(POINTER_CELLS + 4);
    let max_rows = max_rows.max(1);
    let pages_in = hard_pages(&message.pieces);
    let last = pages_in.len().saturating_sub(1);
    let mut pages: Vec<Page> = Vec::new();
    for (index, lines) in pages_in.iter().enumerate() {
        let choices = (index == last).then_some(message.choice).flatten().and_then(|c| choice_range(c, lines.len()));
        let (before, mine, after): (&[HardLine], &[HardLine], &[HardLine]) = match &choices {
            Some(r) => (lines.get(..r.start).unwrap_or(&[]), lines.get(r.clone()).unwrap_or(&[]), lines.get(r.end..).unwrap_or(&[])),
            None => (lines.as_slice(), &[], &[]),
        };
        let wrap = |ls: &[HardLine]| -> Vec<Row> { ls.iter().flat_map(|l| wrap_line(l, max_cells)).collect() };
        let (before, after) = (wrap(before), wrap(after));
        let mine: Vec<Row> = mine.iter().map(|l| choice_row(l, max_cells - POINTER_CELLS)).collect();
        let mut chunks: Vec<Vec<Row>> = before.chunks(max_rows).map(<[Row]>::to_vec).collect();
        if choices.is_some() {
            let c = mine.len();
            match chunks.last_mut() {
                // The choices go under the last of the question's rows when both fit together.
                Some(tail) if tail.len() + c <= max_rows => {
                    let start = tail.len();
                    tail.extend(mine);
                    pages.extend(chunks.drain(..chunks.len().saturating_sub(1)).map(|rows| Page { rows, choices: None }));
                    pages.extend(chunks.pop().map(|rows| Page { rows, choices: Some(start..start + c) }));
                }
                _ => {
                    pages.extend(chunks.drain(..).map(|rows| Page { rows, choices: None }));
                    pages.push(Page { rows: mine, choices: Some(0..c) });
                }
            }
        } else {
            pages.extend(chunks.drain(..).map(|rows| Page { rows, choices: None }));
        }
        pages.extend(after.chunks(max_rows).map(|rows| Page { rows: rows.to_vec(), choices: None }));
    }
    // An empty message is one empty page: the window still shows.
    if pages.is_empty() {
        pages.push(Page::default());
    }
    let cells = pages
        .iter()
        .flat_map(|p| p.rows.iter().enumerate().map(move |(i, r)| r.cells() + usize::from(p.choices.as_ref().is_some_and(|c| c.contains(&i))) * POINTER_CELLS))
        .max()
        .unwrap_or(0);
    let rows = pages.iter().map(|p| p.rows.len()).max().unwrap_or(1).clamp(1, max_rows);
    Laid { pages, cells, rows }
}

/// The lines of the last page that are choices: `first..=last` clamped to what exists (`None` when
/// no line of the text is one).
fn choice_range(choice: Choice, lines: usize) -> Option<Range<usize>> {
    let end = choice.last.saturating_add(1).min(lines);
    (choice.first < end).then_some(choice.first..end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmv_ff7::script::dialogue::MessageOrigin;

    fn msg(pieces: Vec<Piece>) -> Message {
        Message::new(MessageOrigin::Added { layer: "test".into() }, pieces)
    }

    fn text(t: &str) -> Piece {
        Piece::Text(t.into())
    }

    fn rows_of(page: &Page) -> Vec<String> {
        page.rows.iter().map(Row::plain).collect()
    }

    #[test]
    fn folding_gives_the_font_what_it_can_draw_and_nothing_else() {
        assert_eq!(fold("plain ~!"), "plain ~!");
        assert_eq!(fold("\u{201c}Wow\u{201d} \u{2018}x\u{2019}\u{2026}"), "\"Wow\" 'x'...");
        assert_eq!(fold("caf\u{e9} \u{c4}\u{df}\u{e6} \u{f1}"), "cafe Assae n");
        assert_eq!(fold("a\u{4e2d}b\u{1f600}"), "a?b?", "anything unknown is a question mark, never a box");
        assert_eq!(fold("a\tb\nc\u{1b}"), "abc", "control characters take no room");
        // Every character the game's own table can give folds to something drawable.
        for c in ('\u{0}'..='\u{3000}').chain(['\u{2026}', '\u{201c}', '\u{1f600}', '\u{10ffff}']) {
            assert!(fold(&c.to_string()).chars().all(is_drawable), "{c:?}");
        }
    }

    #[test]
    fn short_lines_stay_as_the_game_broke_them_and_pages_follow_page_breaks() {
        let m = msg(vec![text("Biggs"), Piece::NewLine, text("Hello there"), Piece::NewPage, text("Bye")]);
        let laid = lay_out(&m, 40, 4);
        assert_eq!(laid.pages.len(), 2);
        assert_eq!(rows_of(&laid.pages[0]), ["Biggs", "Hello there"]);
        assert_eq!(rows_of(&laid.pages[1]), ["Bye"]);
        assert_eq!((laid.cells, laid.rows), (11, 2));
        assert!(laid.pages.iter().all(|p| p.choices.is_none()));
    }

    #[test]
    fn long_lines_wrap_at_spaces_keep_their_colours_and_their_indent() {
        let m = msg(vec![
            text(" "),
            Piece::Colour(TextColour::Cyan),
            text("one two"),
            Piece::Colour(TextColour::White),
            text(" three four five"),
        ]);
        let laid = lay_out(&m, 12, 8);
        let rows = rows_of(&laid.pages[0]);
        assert_eq!(rows, [" one two", " three four", " five"]);
        assert!(rows.iter().all(|r| r.chars().count() <= 12));
        // The first row's cyan stays cyan, the rest white.
        let first = &laid.pages[0].rows[0];
        assert_eq!(first.runs.iter().find(|r| r.text.contains("one")).map(|r| r.colour), Some(TextColour::Cyan));
        assert_eq!(laid.pages[0].rows[1].runs.last().map(|r| r.colour), Some(TextColour::White));
        // A word wider than a row is cut into pieces, nothing is lost.
        let m = msg(vec![text("abcdefghij")]);
        assert_eq!(rows_of(&lay_out(&m, 4 + POINTER_CELLS, 8).pages[0]), ["abcdef", "ghij"]);
    }

    #[test]
    fn more_rows_than_the_window_has_become_more_pages() {
        let m = msg(vec![text("1"), Piece::NewLine, text("2"), Piece::NewLine, text("3"), Piece::NewLine, text("4"), Piece::NewLine, text("5")]);
        let laid = lay_out(&m, 20, 2);
        assert_eq!(laid.pages.iter().map(rows_of).collect::<Vec<_>>(), [vec!["1", "2"], vec!["3", "4"], vec!["5"]]);
        assert_eq!(laid.rows, 2);
    }

    fn question(first: usize, last: usize) -> Message {
        let mut m = msg(vec![
            text("Q?"),
            Piece::NewLine,
            Piece::Choice,
            text("Yes"),
            Piece::NewLine,
            text("  "),
            Piece::Choice,
            text("No"),
        ]);
        m.choice = Some(Choice { first, last, cursor: first });
        m
    }

    #[test]
    fn choices_stay_with_their_question_when_they_fit_and_lose_their_leading_blanks() {
        let laid = lay_out(&question(1, 2), 30, 4);
        assert_eq!(laid.pages.len(), 1);
        assert_eq!(rows_of(&laid.pages[0]), ["Q?", "Yes", "No"]);
        assert_eq!(laid.pages[0].choices, Some(1..3));
        // The pointer column counts towards the width of a choice row.
        assert_eq!(laid.cells, 3 + POINTER_CELLS);
    }

    #[test]
    fn choices_that_do_not_fit_under_the_question_get_their_own_page() {
        let laid = lay_out(&question(1, 2), 30, 2);
        assert_eq!(laid.pages.len(), 2);
        assert_eq!(rows_of(&laid.pages[0]), ["Q?"]);
        assert_eq!(laid.pages[0].choices, None);
        assert_eq!(rows_of(&laid.pages[1]), ["Yes", "No"]);
        assert_eq!(laid.pages[1].choices, Some(0..2));
    }

    #[test]
    fn a_list_longer_than_the_window_keeps_all_its_rows_together_to_scroll() {
        let mut pieces = vec![text("Where?")];
        for n in 0..13 {
            pieces.push(Piece::NewLine);
            pieces.push(Piece::Choice);
            pieces.push(text(&format!("Place {n}")));
        }
        let mut m = msg(pieces);
        m.choice = Some(Choice { first: 1, last: 13, cursor: 1 });
        let laid = lay_out(&m, 30, 6);
        assert_eq!(laid.pages.len(), 2, "the question, then the 13 choices on one page");
        assert_eq!(laid.pages[1].rows.len(), 13);
        assert_eq!(laid.pages[1].choices, Some(0..13));
        assert_eq!(laid.rows, 6, "the window is as tall as it may be, the page scrolls");
    }

    #[test]
    fn choice_lines_are_counted_on_the_last_page_and_in_hard_lines() {
        // "Intro" NEWPAGE "Q?" / Yes / No: the choices are lines 1 and 2 of the last page. A long
        // question line wraps to two rows, which does not move the numbering.
        let mut m = msg(vec![
            text("Intro"),
            Piece::NewPage,
            text("a question that is far too long for one row"),
            Piece::NewLine,
            text("Yes"),
            Piece::NewLine,
            text("No"),
        ]);
        m.choice = Some(Choice { first: 1, last: 2, cursor: 1 });
        let laid = lay_out(&m, 12, 8);
        let last = laid.pages.last().unwrap();
        let choices: Vec<String> = last.choices.clone().unwrap().map(|i| last.rows[i].plain()).collect();
        assert_eq!(choices, ["Yes", "No"]);
        assert!(last.rows.len() > 3, "the question wrapped");
    }

    #[test]
    fn a_choice_range_the_text_does_not_have_is_clamped_or_makes_a_plain_page() {
        assert_eq!(lay_out(&question(1, 9), 30, 4).pages[0].choices, Some(1..3));
        assert_eq!(lay_out(&question(7, 9), 30, 4).pages[0].choices, None);
    }

    #[test]
    fn a_long_choice_is_cut_not_wrapped() {
        let mut m = msg(vec![text("Q"), Piece::NewLine, text("a very long choice that cannot possibly fit")]);
        m.choice = Some(Choice { first: 1, last: 1, cursor: 1 });
        let laid = lay_out(&m, 16, 4);
        let row = laid.pages[0].rows[1].plain();
        assert_eq!(row.chars().count(), 16 - POINTER_CELLS);
        assert!(row.ends_with("..."));
        assert!(laid.cells <= 16);
    }

    #[test]
    fn blank_messages_and_empty_ones_still_lay_out() {
        assert!(is_blank(&msg(vec![])));
        assert!(is_blank(&msg(vec![Piece::Colour(TextColour::Red), text("  "), Piece::NewLine])));
        assert!(!is_blank(&msg(vec![text("x")])));
        assert!(!is_blank(&question(1, 2)));
        let laid = lay_out(&msg(vec![]), 20, 4);
        assert_eq!(laid.pages.len(), 1);
        assert_eq!((laid.cells, laid.rows), (0, 1));
    }

    #[test]
    fn nothing_ever_exceeds_the_window_for_random_messages_and_sizes() {
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move |n: usize| {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            usize::try_from(state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 33).unwrap() % n.max(1)
        };
        let words = ["a", "Biggs", "Wow!", "\u{201c}quote\u{201d}", "supercalifragilisticexpialidocious", "", "  ", "caf\u{e9}", "\u{2026}"];
        for _ in 0..3000 {
            let mut pieces = Vec::new();
            for _ in 0..next(12) {
                pieces.push(match next(7) {
                    0 => Piece::NewLine,
                    1 => Piece::NewPage,
                    2 => Piece::Choice,
                    3 => Piece::Colour([TextColour::Red, TextColour::White, TextColour::Cyan][next(3)]),
                    _ => text(words[next(words.len())]),
                });
            }
            let mut m = msg(pieces);
            if next(2) == 0 {
                let first = next(8);
                m.choice = Some(Choice { first, last: first + next(10), cursor: first });
            }
            let (cells, rows) = (next(60), next(10));
            let laid = lay_out(&m, cells, rows);
            let limit = cells.max(POINTER_CELLS + 4);
            for page in &laid.pages {
                for (i, row) in page.rows.iter().enumerate() {
                    let pointer = page.choices.as_ref().is_some_and(|c| c.contains(&i));
                    assert!(row.cells() + if pointer { POINTER_CELLS } else { 0 } <= limit, "{row:?} in {limit}");
                    assert!(row.plain().chars().all(is_drawable), "{row:?}");
                }
                if let Some(c) = &page.choices {
                    assert!(c.start < c.end && c.end <= page.rows.len());
                } else {
                    assert!(page.rows.len() <= rows.max(1), "{} rows in {rows}", page.rows.len());
                }
            }
            assert!(laid.cells <= limit && laid.rows >= 1 && laid.rows <= rows.max(1));
        }
    }
}
