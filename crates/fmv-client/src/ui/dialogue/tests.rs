//! Tests of the dialogue window, its keys and the talk controller, with canned conversations (no
//! game data, no GPU).

use super::*;
use crate::ui::font::is_drawable;
use crate::ui::testkit::Rng;
use crate::ui::widgets::{ACCENT, BORDER};
use fmv_ff7::script::dialogue::{Choice, End, MessageOrigin, Piece};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

type Answers = Rc<RefCell<Vec<Option<usize>>>>;

/// A source that hands out canned beats and remembers the answer each `next` was given.
struct Canned {
    beats: VecDeque<Beat>,
    answers: Answers,
}

impl MessageSource for Canned {
    fn next(&mut self, answer: Option<usize>) -> Beat {
        self.answers.borrow_mut().push(answer);
        self.beats.pop_front().unwrap_or(Beat::End(End::Finished))
    }
}

fn source(messages: Vec<Message>) -> (Box<dyn MessageSource>, Answers) {
    let answers: Answers = Rc::default();
    let beats = messages.into_iter().map(Beat::Message).chain([Beat::End(End::Finished)]).collect();
    (Box::new(Canned { beats, answers: Rc::clone(&answers) }), answers)
}

fn canon(pieces: Vec<Piece>) -> Message {
    Message::new(MessageOrigin::Canon { entity: 0, dialog: 0 }, pieces)
}

fn text(t: &str) -> Piece {
    Piece::Text(t.into())
}

fn lines(n: usize) -> Message {
    let mut pieces = Vec::new();
    for i in 0..n {
        if i > 0 {
            pieces.push(Piece::NewLine);
        }
        pieces.push(text(&format!("line {i}")));
    }
    canon(pieces)
}

fn question(options: usize, cursor: usize) -> Message {
    let mut pieces = vec![text("Which one?")];
    for i in 0..options {
        pieces.push(Piece::NewLine);
        pieces.push(Piece::Choice);
        pieces.push(text(&format!("Option {i}")));
    }
    let mut m = canon(pieces);
    m.choice = Some(Choice { first: 1, last: options, cursor: 1 + cursor });
    m
}

const SIZE: (u32, u32) = (1100, 680);
const TINY: (u32, u32) = (320, 200);

fn open(messages: Vec<Message>) -> (DialogueWindow, Answers) {
    let (source, answers) = source(messages);
    (DialogueWindow::open(source).expect("something to say"), answers)
}

fn drawn(window: &DialogueWindow, size: (u32, u32)) -> DrawList {
    let mut list = DrawList::new();
    window.draw(size.0, size.1, 0.0, &mut list);
    list
}

/// The strings drawn in a colour other than the shadow's, in drawing order.
fn said(list: &DrawList) -> Vec<String> {
    list.texts().iter().filter(|t| t.color != crate::ui::widgets::SHADOW).map(|t| t.text.clone()).collect()
}

// ---- keys --------------------------------------------------------------------------------------

#[test]
fn keys_map_one_to_one_and_only_a_fresh_press_confirms_or_closes() {
    for (code, want) in [
        (KeyCode::Enter, DialogueKey::Confirm),
        (KeyCode::NumpadEnter, DialogueKey::Confirm),
        (KeyCode::Space, DialogueKey::Confirm),
        (KeyCode::Backspace, DialogueKey::Back),
        (KeyCode::Escape, DialogueKey::Back),
        (KeyCode::ArrowUp, DialogueKey::Up),
        (KeyCode::KeyW, DialogueKey::Up),
        (KeyCode::ArrowDown, DialogueKey::Down),
        (KeyCode::KeyS, DialogueKey::Down),
    ] {
        assert_eq!(dialogue_key(code, true, false), Some(want), "{code:?}");
        assert_eq!(dialogue_key(code, false, false), None, "a release is never ours: {code:?}");
        let repeats = matches!(want, DialogueKey::Up | DialogueKey::Down);
        assert_eq!(dialogue_key(code, true, true), repeats.then_some(want), "{code:?} repeating");
    }
    for code in [KeyCode::KeyA, KeyCode::KeyD, KeyCode::ArrowLeft, KeyCode::ShiftLeft, KeyCode::KeyP, KeyCode::F1, KeyCode::KeyB] {
        assert_eq!(dialogue_key(code, true, false), None, "{code:?}");
    }
}

// ---- the window --------------------------------------------------------------------------------

#[test]
fn a_window_pages_through_a_message_then_asks_the_source_for_the_next() {
    let two_pages = canon(vec![text("one"), Piece::NewPage, text("two")]);
    let (mut w, answers) = open(vec![two_pages, lines(1)]);
    assert_eq!(w.position(), (0, 0));
    assert!(w.key(DialogueKey::Confirm, SIZE));
    assert_eq!(w.position(), (1, 0), "the second page of the same message");
    assert!(w.key(DialogueKey::Confirm, SIZE));
    assert_eq!(w.position(), (0, 0));
    assert!(drawn(&w, SIZE).says("line 0"), "the next message is on show");
    assert!(!w.key(DialogueKey::Confirm, SIZE), "the source ended: the window closes");
    assert_eq!(*answers.borrow(), [None, None, None], "nothing was asked, so nothing was answered");
}

#[test]
fn rows_beyond_what_a_small_window_can_show_become_pages_of_their_own() {
    let (mut w, _) = open(vec![lines(12)]);
    let mut pages = 1;
    while w.position().0 + 1 < 99 {
        if !w.key(DialogueKey::Confirm, TINY) {
            break;
        }
        if w.position().0 == 0 {
            break;
        }
        pages += 1;
    }
    assert!(pages >= 2, "twelve lines do not fit one page of a 320x200 screen");
    // On a big screen they are one page.
    let (mut w, _) = open(vec![lines(12)]);
    assert!(!w.key(DialogueKey::Confirm, SIZE), "closes on the first Confirm: one page");
}

#[test]
fn back_closes_the_whole_conversation() {
    let (mut w, answers) = open(vec![lines(1), lines(2), lines(3)]);
    assert!(!w.key(DialogueKey::Back, SIZE));
    assert_eq!(answers.borrow().len(), 1, "the rest of the conversation was never asked for");
}

#[test]
fn a_question_moves_its_cursor_wraps_and_answers_with_the_line_number() {
    let (mut w, answers) = open(vec![question(3, 0), lines(1)]);
    assert_eq!(w.position().1, 0);
    assert!(w.on_choices(SIZE), "the pointer is on screen");
    assert!(w.key(DialogueKey::Down, SIZE));
    assert_eq!(w.position().1, 1);
    assert!(w.key(DialogueKey::Down, SIZE) && w.key(DialogueKey::Down, SIZE));
    assert_eq!(w.position().1, 0, "wrapped round the three options");
    assert!(w.key(DialogueKey::Up, SIZE));
    assert_eq!(w.position().1, 2, "and back up through the top");
    // The window shows the pointer on the third option; Confirm answers with its line (3 = 1 + 2).
    assert!(w.key(DialogueKey::Confirm, SIZE));
    assert_eq!(*answers.borrow(), [None, Some(3)]);
    assert!(drawn(&w, SIZE).says("line 0"));
}

#[test]
fn a_question_starts_where_the_script_put_the_cursor() {
    let (w, _) = open(vec![question(3, 2)]);
    assert_eq!(w.position().1, 2);
    // A cursor that is not a choice is the first.
    let mut m = question(3, 0);
    m.choice = Some(Choice { first: 1, last: 3, cursor: 99 });
    let (w, _) = open(vec![m]);
    assert!(w.position().1 <= 2);
}

#[test]
fn the_arrows_do_nothing_on_a_page_without_choices_and_the_choices_wait_for_their_page() {
    // On a big screen the question and its options share a page.
    let (mut w, answers) = open(vec![question(4, 1), lines(1)]);
    let big = drawn(&w, SIZE);
    assert!(big.says("Which one?") && big.says("Option 3"));
    // On a small one they do not: the question is a page of its own, and the arrows do nothing on it
    // (the cursor stays where the script put it) until Confirm turns to the options.
    let (mut small, small_answers) = open(vec![question(6, 2), lines(1)]);
    let first = drawn(&small, TINY);
    assert!(first.says("Which one?") && !first.says("Option 0"), "{:?}", said(&first));
    for _ in 0..3 {
        assert!(small.key(DialogueKey::Down, TINY));
    }
    assert_eq!(small.position(), (0, 2));
    assert!(small.key(DialogueKey::Confirm, TINY));
    assert_eq!(small.position(), (1, 2), "the options' page, the pointer where it was");
    assert!(drawn(&small, TINY).says("Option 5"));
    assert_eq!(small_answers.borrow().len(), 1, "nothing was answered yet");
    // Confirm on the options answers with the line of the option the pointer is on (option n is line n + 1).
    assert!(small.key(DialogueKey::Confirm, TINY));
    assert_eq!(small_answers.borrow().last().copied().flatten(), Some(3));
    assert!(w.key(DialogueKey::Confirm, SIZE));
    assert_eq!(answers.borrow().last().copied().flatten(), Some(2), "the cursor was on option 1: line 2");
}

#[test]
fn a_long_list_scrolls_to_keep_the_pointer_in_view() {
    let (mut w, answers) = open(vec![question(13, 0), lines(1)]);
    let size = TINY;
    let top = said(&drawn(&w, size));
    assert!(!top.iter().any(|t| t == "Option 12"), "{top:?}");
    // Down to the last option: the list has scrolled to show it and no longer shows the first.
    let mut moved = 0;
    while !said(&drawn(&w, size)).iter().any(|t| t == "Option 12") {
        // (the question may have its own page on a small screen: Confirm moves on to the options)
        if w.position().1 == 0 && moved == 0 && !said(&drawn(&w, size)).iter().any(|t| t.starts_with("Option")) {
            assert!(w.key(DialogueKey::Confirm, size));
        }
        assert!(w.key(DialogueKey::Down, size));
        moved += 1;
        assert!(moved < 40, "the last option never came into view");
    }
    assert_eq!(w.position().1, 12.min(moved), "the cursor is on it");
    assert!(!said(&drawn(&w, size)).iter().any(|t| t == "Option 0"));
    // Wrapping from the last goes to the first, which scrolls back.
    assert!(w.key(DialogueKey::Down, size));
    assert_eq!(w.position().1, 0);
    assert!(said(&drawn(&w, size)).iter().any(|t| t == "Option 0"));
    // And the answer is the line of the option the pointer is on (option n is line n + 1).
    assert!(w.key(DialogueKey::Up, size));
    assert!(w.key(DialogueKey::Confirm, size));
    assert_eq!(answers.borrow().last().copied().flatten(), Some(13));
}

#[test]
fn blank_messages_are_skipped_and_a_source_with_nothing_opens_no_window() {
    let blank = || canon(vec![Piece::Colour(TextColour::Red), text("   "), Piece::NewLine]);
    let (w, _) = open(vec![blank(), blank(), lines(1)]);
    assert!(drawn(&w, SIZE).says("line 0"));
    let (src, _) = source(vec![]);
    assert!(DialogueWindow::open(src).is_none());
    let (src, _) = source(vec![blank(); 9]);
    assert!(DialogueWindow::open(src).is_none(), "nine blanks in a row is a damaged script, not a conversation");
}

#[test]
fn an_added_line_is_bordered_apart_and_says_the_same_things() {
    let canonical = open(vec![lines(2)]).0;
    let added = open(vec![Message::new(MessageOrigin::Added { layer: "x".into() }, vec![text("line 0"), Piece::NewLine, text("line 1")])]).0;
    let (a, b) = (drawn(&canonical, SIZE), drawn(&added, SIZE));
    assert_eq!(a.quads()[0].color, BORDER);
    assert_eq!(b.quads()[0].color, ACCENT);
    assert_eq!(said(&a), said(&b));
    assert_eq!((a.quads()[0].x, a.quads()[0].w), (b.quads()[0].x, b.quads()[0].w), "the same box");
}

#[test]
fn the_text_is_the_games_folded_to_what_the_font_draws() {
    let (w, _) = open(vec![canon(vec![text("\u{201c}Wow\u{201d} caf\u{e9}\u{2026}")])]);
    let list = drawn(&w, SIZE);
    assert!(list.says("\"Wow\" cafe..."), "{:?}", said(&list));
    assert!(said(&list).iter().all(|t| t.chars().all(is_drawable)));
}

#[test]
fn the_window_sits_inside_the_screen_above_the_hud_at_every_size() {
    let sizes = [(320, 200), (320, 240), (640, 360), (800, 600), (1100, 680), (1280, 720), (1920, 1080), (2560, 1440), (3840, 2160), (321, 241), (1000, 300)];
    for message in [lines(4), question(13, 0), canon(vec![text("one"), Piece::NewPage, text("two")])] {
        let (w, _) = open(vec![message]);
        for (vw, vh) in sizes {
            let list = drawn(&w, (vw, vh));
            let border = list.quads()[0];
            let (vw, vh) = (i32::try_from(vw).unwrap(), i32::try_from(vh).unwrap());
            assert!(border.x >= 0 && border.y >= 0 && border.x + border.w <= vw && border.y + border.h <= vh, "{border:?} in {vw}x{vh}");
            let hud = 36 * i32::try_from(hud_scale(u32::try_from(vw).unwrap(), u32::try_from(vh).unwrap())).unwrap();
            if vh >= 240 {
                assert!(border.y + border.h <= vh - hud, "{border:?} over the HUD in {vw}x{vh}");
            }
            for q in &list.quads()[1..] {
                // The blinking arrow hangs two units below the border by design.
                let slack = 2 * geometry(vw as u32, vh as u32).s;
                assert!(q.x >= border.x && q.x + q.w <= border.x + border.w && q.y >= border.y && q.y + q.h <= border.y + border.h + slack, "{q:?} outside {border:?} in {vw}x{vh}");
            }
        }
    }
}

#[test]
fn nothing_the_game_can_say_is_clipped_or_drawn_as_a_box() {
    let mut rng = Rng::new(7);
    let words = ["a", "Biggs", "Wow!", "\u{201c}quote\u{201d}", "supercalifragilisticexpialidocious and then some more words", "", "   ", "caf\u{e9}", "\u{2026}", "\u{4e2d}"];
    for _ in 0..1500 {
        let mut pieces = Vec::new();
        for _ in 0..rng.below(14) {
            pieces.push(match rng.below(7) {
                0 => Piece::NewLine,
                1 => Piece::NewPage,
                2 => Piece::Choice,
                3 => Piece::Colour([TextColour::Red, TextColour::White, TextColour::Rainbow, TextColour::Flash][rng.below(4)]),
                _ => text(words[rng.below(words.len())]),
            });
        }
        let mut m = canon(pieces);
        if rng.chance(2) {
            let first = rng.below(6);
            m.choice = Some(Choice { first, last: first + rng.below(9), cursor: first + rng.below(5) });
        }
        let (vw, vh) = (u32::try_from(100 + rng.below(2200)).unwrap(), u32::try_from(100 + rng.below(1400)).unwrap());
        let Some(mut w) = DialogueWindow::open(source(vec![m, lines(2)]).0) else { continue };
        for _ in 0..12 {
            let list = drawn(&w, (vw, vh));
            assert!(list.texts().iter().all(|t| t.text.chars().all(is_drawable)), "{:?}", said(&list));
            let key = [DialogueKey::Confirm, DialogueKey::Up, DialogueKey::Down, DialogueKey::Confirm][rng.below(4)];
            if !w.key(key, (vw, vh)) {
                break;
            }
        }
    }
}

// ---- who is in front of us ---------------------------------------------------------------------

fn spot(x: f32, y: f32, radius: f32) -> Spot {
    Spot { pos: Pos::new(x, y, 0.0), radius }
}

#[test]
fn the_nearest_character_in_reach_and_in_front_is_talked_to() {
    let own = Pos::new(0.0, 0.0, 0.0);
    let east = 0.0_f32;
    // In reach and straight ahead; behind; out of reach; to the side; very near but behind.
    assert_eq!(pick_target(own, east, &[spot(50.0, 0.0, 80.0)]), Some(0));
    assert_eq!(pick_target(own, east, &[spot(-50.0, 0.0, 80.0)]), None, "behind the player: not in front");
    assert_eq!(pick_target(own, east, &[spot(90.0, 0.0, 80.0)]), None, "out of reach");
    assert_eq!(pick_target(own, east, &[spot(0.0, 50.0, 80.0)]), None, "square to the side is 90 degrees off");
    assert_eq!(pick_target(own, east, &[spot(40.0, 40.0, 80.0)]), Some(0), "45 degrees off is in front");
    assert_eq!(pick_target(own, east, &[spot(-20.0, 0.0, 80.0)]), Some(0), "so near that facing does not matter");
    assert_eq!(pick_target(own, east, &[spot(-40.0, 0.0, 80.0)]), None, "half the radius behind is not that near");
    // The nearest wins; a character with no reach is nobody.
    assert_eq!(pick_target(own, east, &[spot(70.0, 0.0, 80.0), spot(30.0, 5.0, 80.0), spot(10.0, 0.0, 0.0)]), Some(1));
    // The heading counts: facing north, the character to the north is the one.
    let north = std::f32::consts::FRAC_PI_2;
    assert_eq!(pick_target(own, north, &[spot(50.0, 0.0, 80.0), spot(0.0, 60.0, 80.0)]), Some(1));
    // Heights are ignored.
    let high = Spot { pos: Pos::new(50.0, 0.0, 5000.0), radius: 80.0 };
    assert_eq!(pick_target(own, east, &[high]), Some(0));
}

#[test]
fn what_is_not_a_number_is_nobody() {
    let own = Pos::new(0.0, 0.0, 0.0);
    assert_eq!(pick_target(own, f32::NAN, &[spot(10.0, 0.0, 80.0)]), None);
    assert_eq!(pick_target(Pos::new(f32::NAN, 0.0, 0.0), 0.0, &[spot(10.0, 0.0, 80.0)]), None);
    assert_eq!(pick_target(own, 0.0, &[spot(f32::NAN, 0.0, 80.0), spot(10.0, f32::INFINITY, 80.0), spot(10.0, 0.0, f32::NAN)]), None);
    assert_eq!(pick_target(own, 0.0, &[spot(10.0, 0.0, 80.0)]), Some(0));
    assert_eq!(pick_target(own, 0.0, &[]), None);
    // On the very spot of the player: in front, whatever the heading.
    assert_eq!(pick_target(own, 1.0, &[spot(0.0, 0.0, 80.0)]), Some(0));
}

// ---- the controller ----------------------------------------------------------------------------

/// Conversations by NPC name: those with a script of messages have something to say.
struct Fake {
    radius: f32,
    says: Vec<(&'static str, Vec<Message>)>,
    started: Rc<RefCell<Vec<String>>>,
    /// How many times the story hook was set.
    stories: Rc<Cell<u32>>,
}

impl Conversations for Fake {
    fn set_story(&mut self, _story: ff7::StoryFactory) {
        self.stories.set(self.stories.get() + 1);
    }

    fn talk_radius(&mut self, _area: &str, npc: &NpcView) -> f32 {
        if self.says.iter().any(|(n, _)| *n == npc.name) {
            self.radius
        } else {
            0.0
        }
    }

    fn start(&mut self, area: &str, npc: &NpcView) -> Option<Box<dyn MessageSource>> {
        self.started.borrow_mut().push(format!("{area}/{}", npc.name));
        let (_, messages) = self.says.iter().find(|(n, _)| *n == npc.name)?;
        Some(source(messages.clone()).0)
    }
}

fn npc(id: u32, name: &str, x: f32, y: f32) -> NpcView {
    NpcView { id, name: name.into(), model: "aaaa".into(), idle_animation: None, pos: Pos::new(x, y, 0.0), facing: 0.0 }
}

fn talk_with(says: Vec<(&'static str, Vec<Message>)>) -> (Talk, Rc<RefCell<Vec<String>>>) {
    let started: Rc<RefCell<Vec<String>>> = Rc::default();
    (Talk::new(Box::new(Fake { radius: 80.0, says, started: Rc::clone(&started), stories: Rc::default() })), started)
}

fn around<'a>(npcs: &'a [NpcView], own: (f32, f32), facing: f32) -> Around<'a> {
    Around { area: "md1stin", own: Pos::new(own.0, own.1, 0.0), facing, npcs, blocked: false }
}

fn press(t: &mut Talk, code: KeyCode, a: &Around<'_>) -> bool {
    t.key(code, true, false, a)
}

#[test]
fn enter_next_to_a_character_in_front_of_us_opens_its_dialogue_and_the_player_freezes() {
    let npcs = [npc(0, "gu0", 60.0, 0.0), npc(1, "mute", 20.0, 5.0)];
    let (mut talk, started) = talk_with(vec![("gu0", vec![lines(1), lines(2)])]);
    let a = around(&npcs, (0.0, 0.0), 0.0);
    assert!(!talk.is_open());
    assert!(press(&mut talk, KeyCode::Enter, &a));
    assert!(talk.is_open());
    assert_eq!(*started.borrow(), ["md1stin/gu0"], "the nearer one has nothing to say and was never even picked");
    let mut list = DrawList::new();
    talk.draw(1100, 680, 0.0, &mut list);
    assert!(list.says("line 0"));
    // Page through and out: the second key shows the next message, the third ends it.
    assert!(press(&mut talk, KeyCode::Space, &a) && talk.is_open());
    let mut list = DrawList::new();
    talk.draw(1100, 680, 0.0, &mut list);
    assert!(list.says("line 1"), "the second message has two lines");
    assert!(press(&mut talk, KeyCode::NumpadEnter, &a));
    assert!(!talk.is_open(), "the conversation ended and took its Enter");
    // ... and can be started again.
    assert!(press(&mut talk, KeyCode::Enter, &a) && talk.is_open());
}

#[test]
fn nobody_near_nobody_in_front_and_nobody_with_anything_to_say_leaves_the_key_to_the_rest() {
    let (mut talk, _) = talk_with(vec![("gu0", vec![lines(1)])]);
    let near = [npc(0, "gu0", 60.0, 0.0)];
    assert!(!press(&mut talk, KeyCode::Enter, &around(&near, (0.0, 0.0), std::f32::consts::PI)), "behind us");
    let far = [npc(0, "gu0", 600.0, 0.0)];
    assert!(!press(&mut talk, KeyCode::Enter, &around(&far, (0.0, 0.0), 0.0)), "out of reach");
    let mute = [npc(0, "mute", 60.0, 0.0)];
    assert!(!press(&mut talk, KeyCode::Enter, &around(&mute, (0.0, 0.0), 0.0)), "nothing to say");
    assert!(!press(&mut talk, KeyCode::Enter, &around(&[], (0.0, 0.0), 0.0)), "nobody here");
    // A conversation that says nothing at all (every message blank) opens no window.
    let blank = canon(vec![text("  ")]);
    let (mut talk, _) = talk_with(vec![("gu0", vec![blank])]);
    assert!(!press(&mut talk, KeyCode::Enter, &around(&near, (0.0, 0.0), 0.0)));
    assert!(!talk.is_open());
}

#[test]
fn a_battle_owns_enter_and_closes_a_dialogue_that_is_open() {
    let npcs = [npc(0, "gu0", 60.0, 0.0)];
    let (mut talk, started) = talk_with(vec![("gu0", vec![lines(1), lines(1)])]);
    let blocked = Around { blocked: true, ..around(&npcs, (0.0, 0.0), 0.0) };
    assert!(!press(&mut talk, KeyCode::Enter, &blocked), "the battle's panel takes Enter");
    assert!(started.borrow().is_empty());
    let free = around(&npcs, (0.0, 0.0), 0.0);
    assert!(press(&mut talk, KeyCode::Enter, &free) && talk.is_open());
    assert!(!press(&mut talk, KeyCode::Enter, &blocked), "a battle began: the dialogue closed and the key is the battle's");
    assert!(!talk.is_open());
}

#[test]
fn while_open_the_dialogue_takes_its_keys_and_lets_the_rest_through() {
    let npcs = [npc(0, "gu0", 60.0, 0.0)];
    let (mut talk, _) = talk_with(vec![("gu0", vec![question(2, 0), lines(1)])]);
    let a = around(&npcs, (0.0, 0.0), 0.0);
    // Closed: only a fresh Enter or Space is ours.
    for code in [KeyCode::Backspace, KeyCode::Escape, KeyCode::ArrowUp, KeyCode::ArrowDown, KeyCode::KeyW] {
        assert!(!press(&mut talk, code, &a), "{code:?} is not ours while closed");
    }
    assert!(!talk.key(KeyCode::Enter, false, false, &a), "a release");
    assert!(!talk.key(KeyCode::Enter, true, true, &a), "a held key does not start a conversation");
    assert!(press(&mut talk, KeyCode::Enter, &a));
    // Open: movement presses and anything else go on to their own handling, releases always do.
    for code in [KeyCode::KeyA, KeyCode::KeyD, KeyCode::ShiftLeft, KeyCode::KeyP, KeyCode::ArrowLeft] {
        assert!(!press(&mut talk, code, &a), "{code:?}");
    }
    assert!(!talk.key(KeyCode::ArrowUp, false, false, &a), "a movement key let go of during a dialogue still reaches the walk");
    assert!(!talk.key(KeyCode::Enter, false, false, &a));
    assert!(!talk.key(KeyCode::Enter, true, true, &a), "a held Enter does not page through a conversation");
    assert!(talk.is_open());
    // The question: a fresh arrow press moves the pointer, Enter answers, Esc closes everything.
    assert!(talk.key(KeyCode::ArrowDown, true, false, &a));
    assert_eq!(talk.window.as_ref().map(|w| w.position().1), Some(1));
    assert!(press(&mut talk, KeyCode::Enter, &a));
    assert!(talk.is_open() && talk.message().is_some_and(|m| m.choice.is_none()));
    assert!(press(&mut talk, KeyCode::Escape, &a), "Esc closes the dialogue instead of arming the quit prompt");
    assert!(!talk.is_open());
}

#[test]
fn a_direction_held_while_the_window_opens_does_not_spin_the_pointer() {
    let npcs = [npc(0, "gu0", 60.0, 0.0)];
    let (mut talk, _) = talk_with(vec![("gu0", vec![question(3, 0), lines(1)])]);
    let a = around(&npcs, (0.0, 0.0), 0.0);
    let pointer = |t: &Talk| t.window.as_ref().map(|w| w.position().1);
    // The player walks up holding W and presses Enter: the window opens with the key still down, and
    // the system goes on sending repeats of it (a pad past its repeat delay does the same).
    assert!(!talk.key(KeyCode::KeyW, true, false, &a), "closed: the walk's");
    assert!(press(&mut talk, KeyCode::Enter, &a));
    for _ in 0..10 {
        assert!(!talk.key(KeyCode::KeyW, true, true, &a), "repeats of a key held before the window opened belong to the walk");
    }
    assert_eq!(pointer(&talk), Some(0), "the pointer stayed where the script put it");
    // A key pressed fresh while the window is open moves it, and its repeats do too.
    assert!(talk.key(KeyCode::KeyW, true, false, &a));
    assert_eq!(pointer(&talk), Some(2), "up from the first option wraps to the last");
    assert!(talk.key(KeyCode::KeyW, true, true, &a));
    assert_eq!(pointer(&talk), Some(1));
    // Let go: the release reaches the walk, and later repeats of that key are not the pointer's again.
    assert!(!talk.key(KeyCode::KeyW, false, false, &a));
    assert!(!talk.key(KeyCode::KeyW, true, true, &a));
    assert_eq!(pointer(&talk), Some(1));
    // The same for the other direction and for another window: nothing stays armed across one.
    assert!(talk.key(KeyCode::ArrowDown, true, false, &a));
    assert!(press(&mut talk, KeyCode::Enter, &a) && press(&mut talk, KeyCode::Enter, &a));
    assert!(!talk.is_open());
    assert!(press(&mut talk, KeyCode::Enter, &a));
    assert!(!talk.key(KeyCode::ArrowDown, true, true, &a), "a key armed for the last window is not armed for this one");
    assert_eq!(pointer(&talk), Some(0));
}

#[test]
fn the_story_hook_reaches_the_conversations() {
    let stories = Rc::new(Cell::new(0));
    let mut talk = Talk::new(Box::new(Fake { radius: 80.0, says: vec![], started: Rc::default(), stories: Rc::clone(&stories) }));
    talk.set_story(Box::new(|| Box::new(fmv_ff7::script::dialogue::StartOfGame)));
    assert_eq!(stories.get(), 1);
}

#[test]
fn a_line_that_is_not_the_games_flows_through_the_same_controller() {
    let added = Message::new(MessageOrigin::Added { layer: "player-lines".into() }, vec![text("Hello, traveller.")]);
    let npcs = [npc(0, "gu0", 60.0, 0.0)];
    let (mut talk, _) = talk_with(vec![("gu0", vec![added])]);
    assert!(press(&mut talk, KeyCode::Enter, &around(&npcs, (0.0, 0.0), 0.0)));
    assert!(talk.message().is_some_and(|m| !m.origin.is_canon()));
    let mut list = DrawList::new();
    talk.draw(1100, 680, 0.0, &mut list);
    assert!(list.says("Hello, traveller."));
    assert_eq!(list.quads()[0].color, ACCENT);
}

#[test]
fn the_game_conversations_without_a_game_have_nothing_to_say() {
    let mut conversations = ff7::Ff7Conversations::new(None);
    let n = npc(0, "gu0", 1.0, 2.0);
    assert_eq!(conversations.talk_radius("md1stin", &n), 0.0);
    assert!(conversations.start("md1stin", &n).is_none());
    let mut talk = Talk::ff7(None);
    assert!(!press(&mut talk, KeyCode::Enter, &around(&[n], (0.0, 0.0), 0.0)));
}

#[test]
fn closing_by_hand_closes() {
    let npcs = [npc(0, "gu0", 60.0, 0.0)];
    let (mut talk, _) = talk_with(vec![("gu0", vec![lines(1)])]);
    assert!(press(&mut talk, KeyCode::Enter, &around(&npcs, (0.0, 0.0), 0.0)));
    talk.close();
    assert!(!talk.is_open() && talk.message().is_none());
    let mut list = DrawList::new();
    talk.draw(1100, 680, 0.0, &mut list);
    assert!(list.is_empty(), "nothing is drawn while closed");
}

// ---- OK is a press the server hears of -----------------------------------------------------------

#[test]
fn a_fresh_ok_in_the_field_is_a_press_that_names_the_character_it_talked_to_and_nobody_else() {
    let npcs = [npc(0, "gu0", 60.0, 0.0), npc(1, "mute", 20.0, 5.0)];
    let (mut talk, _) = talk_with(vec![("gu0", vec![lines(1), lines(1)])]);
    let a = around(&npcs, (0.0, 0.0), 0.0);
    // facing the character that has something to say: the key opens its dialogue and the press names it
    assert!(press(&mut talk, KeyCode::Enter, &a));
    assert_eq!(talk.take_press(), Some(Some(0)));
    assert_eq!(talk.take_press(), None, "a press is taken once");
    // paging through the dialogue is not a press
    assert!(press(&mut talk, KeyCode::Space, &a));
    assert_eq!(talk.take_press(), None);
    assert!(press(&mut talk, KeyCode::Enter, &a) && !talk.is_open());
    assert_eq!(talk.take_press(), None, "the key that ended the conversation is not a press either");
    // nobody in reach: the key is not the dialogue's, and it is a press where the player stands (a ladder, a panel)
    let far = [npc(0, "gu0", 600.0, 0.0)];
    assert!(!press(&mut talk, KeyCode::Enter, &around(&far, (0.0, 0.0), 0.0)));
    assert_eq!(talk.take_press(), Some(None));
    // a character that has nothing to say does not take the press away from the player's feet
    let mute = [npc(1, "mute", 20.0, 5.0)];
    assert!(!press(&mut talk, KeyCode::Enter, &around(&mute, (0.0, 0.0), 0.0)));
    assert_eq!(talk.take_press(), Some(None));
    // nothing else is a press: a key held (repeating), a release, another key, the battle's Enter
    assert!(!talk.key(KeyCode::Enter, true, true, &a));
    assert_eq!(talk.take_press(), None, "a repeat of a held key");
    assert!(!talk.key(KeyCode::Enter, false, false, &a));
    assert_eq!(talk.take_press(), None, "a release");
    assert!(!talk.key(KeyCode::KeyW, true, false, &a));
    assert_eq!(talk.take_press(), None);
    let blocked = Around { blocked: true, ..a };
    assert!(!talk.key(KeyCode::Enter, true, false, &blocked));
    assert_eq!(talk.take_press(), None, "a battle owns Enter");
}

/// Conversations that remember the story each one was started with (the moment it read as `Var[2][0]`).
struct Remembers {
    started: Rc<RefCell<Vec<(String, u16)>>>,
    story: ff7::StoryFactory,
}

fn moment_of(vars: &dyn fmv_ff7::script::dialogue::StoryVars) -> u16 {
    u16::from_le_bytes([vars.byte(2, 0), vars.byte(2, 1)])
}

impl Conversations for Remembers {
    fn talk_radius(&mut self, _area: &str, _npc: &NpcView) -> f32 {
        80.0
    }

    fn start(&mut self, _area: &str, npc: &NpcView) -> Option<Box<dyn MessageSource>> {
        let vars = (self.story)();
        self.started.borrow_mut().push((npc.name.clone(), moment_of(vars.as_ref())));
        Some(Box::new(Reads { vars, said: 0 }))
    }

    fn set_story(&mut self, story: ff7::StoryFactory) {
        self.story = story;
    }
}

/// A conversation of two windows, each saying the moment its story had when the conversation started.
struct Reads {
    vars: Box<dyn fmv_ff7::script::dialogue::StoryVars>,
    said: u32,
}

impl MessageSource for Reads {
    fn next(&mut self, _answer: Option<usize>) -> Beat {
        self.said += 1;
        if self.said > 2 {
            return Beat::End(End::Finished);
        }
        Beat::Message(canon(vec![text(&format!("moment {}", moment_of(self.vars.as_ref())))]))
    }
}

#[test]
fn the_talk_a_press_opens_reads_the_story_before_the_step_the_press_makes_and_the_next_talk_the_story_after() {
    use fmv_core::{AccountId, AreaId, EntityId, GameId};
    use fmv_ff7::story::StoryState;
    use fmv_protocol::{NpcSnapshot, ServerMsg, StoryAdvance};

    let area = AreaId { game: GameId::Fixture, key: "nmkin_3".into() };
    let started: Rc<RefCell<Vec<(String, u16)>>> = Rc::default();
    let mut talk = Talk::new(Box::new(Remembers { started: Rc::clone(&started), story: ff7::story_of(None) }));
    let mut state = ClientState::new(EntityId(1), AccountId(1), Pos::new(0.0, 0.0, 0.0), area.clone());
    // the area's snapshot names the woman 60 units in front of us; the server has told us the story stands at 13
    let woman = NpcSnapshot { id: 3, name: "woman".into(), model: "aaaa".into(), idle_animation: None, pos: Pos::new(60.0, 0.0, 0.0), facing: 0.0 };
    state.apply(0.0, ServerMsg::AreaSnapshot { area, tick: 0, entities: vec![], npcs: vec![woman] });
    state.story.on_update(StoryState::at(13).encode(), None);
    let shown = |talk: &Talk| {
        let mut list = DrawList::new();
        talk.draw(1100, 680, 0.0, &mut list);
        list
    };

    // The key builds the conversation NOW, from the story as the mirror has it (13), and reports the press that the
    // window sends to the server afterwards.
    let key = field_key(&mut talk, &state, "nmkin_3", 0.0, KeyCode::Enter, true, false);
    assert_eq!(key, FieldKey { consumed: true, interact: Some(Some(3)) });
    assert_eq!(*started.borrow(), [("woman".to_string(), 13)]);
    assert!(shown(&talk).says("moment 13"));

    // The server answers the press with the step, and the mirror moves on: the conversation that is open does not.
    state.story.on_update(StoryState::at(14).encode(), Some(StoryAdvance { point: 0, summary: "moment=13->14".into() }));
    assert_eq!(state.story.moment(), Some(14));
    let key = field_key(&mut talk, &state, "nmkin_3", 0.1, KeyCode::Enter, true, false);
    assert_eq!(key, FieldKey { consumed: true, interact: None }, "paging through is no press");
    assert!(shown(&talk).says("moment 13"), "the next window of the same conversation still reads the story it started with");
    let key = field_key(&mut talk, &state, "nmkin_3", 0.2, KeyCode::Enter, true, false);
    assert!(key.consumed && !talk.is_open(), "the conversation ended");
    assert_eq!(started.borrow().len(), 1, "no new conversation was started by the paging");

    // The next talk reads the later story.
    let key = field_key(&mut talk, &state, "nmkin_3", 0.3, KeyCode::Enter, true, false);
    assert_eq!(key, FieldKey { consumed: true, interact: Some(Some(3)) });
    assert_eq!(*started.borrow(), [("woman".to_string(), 13), ("woman".to_string(), 14)]);
    assert!(shown(&talk).says("moment 14"));
}

#[test]
fn an_ok_with_nobody_around_is_a_press_where_the_player_stands_and_another_key_is_nothing() {
    use fmv_core::{AccountId, AreaId, EntityId, GameId};
    let area = AreaId { game: GameId::Fixture, key: "md1stin".into() };
    let started: Rc<RefCell<Vec<(String, u16)>>> = Rc::default();
    let mut talk = Talk::new(Box::new(Remembers { started: Rc::clone(&started), story: ff7::story_of(None) }));
    let state = ClientState::new(EntityId(1), AccountId(1), Pos::new(0.0, 0.0, 0.0), area);
    // no NPCs: a fresh Space is a press where the player stands, and nobody is talked to
    let key = field_key(&mut talk, &state, "md1stin", 0.0, KeyCode::Space, true, false);
    assert_eq!(key, FieldKey { consumed: false, interact: Some(None) });
    assert!(started.borrow().is_empty());
    // a key that is not OK is nothing
    let key = field_key(&mut talk, &state, "md1stin", 0.0, KeyCode::KeyW, true, false);
    assert_eq!(key, FieldKey { consumed: false, interact: None });
}

// ---- the voice hook ----------------------------------------------------------------------------

use crate::mods::{ModSink, VoiceCue};
use std::sync::Mutex;

/// A sink that keeps the cues it is given.
#[derive(Default)]
struct Cues(Mutex<Vec<VoiceCue>>);

impl ModSink for Cues {
    fn cue(&self, cue: VoiceCue) {
        self.0.lock().expect("cues").push(cue);
    }
    fn note(&self, _line: &str) {}
}

impl Cues {
    fn taken(&self) -> Vec<VoiceCue> {
        std::mem::take(&mut *self.0.lock().expect("cues"))
    }
}

/// `m` as the message of dialog number `dialog` (window 0).
fn numbered(dialog: usize, mut m: Message) -> Message {
    m.origin = MessageOrigin::Canon { entity: 0, dialog };
    m
}

#[test]
fn the_window_says_when_the_player_reaches_the_games_next_page_and_not_the_windows_own_cuts() {
    // Two game pages. The first has 12 lines: a tiny window cuts it into several pages of its own.
    let mut pieces: Vec<Piece> = (0..12).flat_map(|i| [Piece::NewLine, text(&format!("line {i}"))]).skip(1).collect();
    pieces.push(Piece::NewPage);
    pieces.push(text("the second page"));
    let cues = Arc::new(Cues::default());
    let (w, _) = open(vec![numbered(7, canon(pieces))]);
    let mut w = w.with_voice("md1stin", cues.clone());
    let mut cuts = 0;
    while w.key(DialogueKey::Confirm, TINY) {
        cuts += 1;
        if cuts > 40 {
            break;
        }
    }
    // (Two presses that kept the window open: three pages of its own, two cut from the first game page and one for the second.)
    assert!(cuts >= 2, "the first game page is cut into several pages of the window's own: {cuts}");
    // Exactly one cue, when the second game page was reached: page 1 of dialog 7 as window 0.
    assert_eq!(cues.taken(), vec![VoiceCue::Say { field: "md1stin".into(), window: 0, dialog: 7, page: 1 }]);
}

#[test]
fn a_questions_pointer_says_the_absolute_line_it_reached_and_stays_quiet_when_it_does_not_move() {
    let cues = Arc::new(Cues::default());
    let (w, _) = open(vec![numbered(4, question(3, 0))]);
    let mut w = w.with_voice("nmkin_3", cues.clone());
    // The question's first choice is line 1; the pointer starts on it. Down: lines 2 and 3, then round to 1; Up: back to 3.
    for key in [DialogueKey::Down, DialogueKey::Down, DialogueKey::Down, DialogueKey::Up] {
        assert!(w.key(key, SIZE));
    }
    let line = |line| VoiceCue::Choice { field: "nmkin_3".into(), dialog: 4, line };
    assert_eq!(cues.taken(), vec![line(2), line(3), line(1), line(3)]);
    // A question with one choice has nowhere to move to.
    let (w, _) = open(vec![numbered(5, question(1, 0))]);
    let mut w = w.with_voice("a", cues.clone());
    assert!(w.key(DialogueKey::Down, SIZE) && w.key(DialogueKey::Up, SIZE));
    assert!(cues.taken().is_empty());
}

#[test]
fn a_window_without_a_voice_says_nothing_and_a_message_that_is_not_the_games_has_no_voice() {
    let cues = Arc::new(Cues::default());
    let (mut plain, _) = open(vec![numbered(7, canon(vec![text("a"), Piece::NewPage, text("b")]))]);
    assert!(plain.key(DialogueKey::Confirm, SIZE));
    assert!(cues.taken().is_empty());
    // A line this project added (not the game's) has no dialog number and no voice.
    let added = Message::new(MessageOrigin::Added { layer: "test".into() }, vec![text("a"), Piece::NewPage, text("b")]);
    let (w, _) = open(vec![added]);
    let mut w = w.with_voice("md1stin", cues.clone());
    assert!(w.key(DialogueKey::Confirm, SIZE));
    assert!(cues.taken().is_empty());
}

/// A conversation of canned messages that says it is voiced, to the sink it was given.
struct Voicey {
    messages: Vec<Message>,
    sink: Arc<Cues>,
}

impl Conversations for Voicey {
    fn talk_radius(&mut self, _area: &str, _npc: &NpcView) -> f32 {
        80.0
    }
    fn start(&mut self, _area: &str, _npc: &NpcView) -> Option<Box<dyn MessageSource>> {
        Some(source(self.messages.clone()).0)
    }
    fn voice(&mut self) -> Option<Arc<dyn ModSink>> {
        Some(self.sink.clone())
    }
}

#[test]
fn a_voiced_conversation_gives_its_window_the_area_that_names_its_voices() {
    let sink = Arc::new(Cues::default());
    let messages = vec![numbered(2, canon(vec![text("one"), Piece::NewPage, text("two")]))];
    let mut talk = Talk::new(Box::new(Voicey { messages, sink: sink.clone() }));
    let npcs = [npc(0, "gu0", 20.0, 0.0)];
    assert!(press(&mut talk, KeyCode::Enter, &around(&npcs, (0.0, 0.0), 0.0)));
    assert!(sink.taken().is_empty(), "the first page's voice is the source's, not the window's");
    assert!(press(&mut talk, KeyCode::Enter, &around(&npcs, (0.0, 0.0), 0.0)));
    assert_eq!(sink.taken(), vec![VoiceCue::Say { field: "md1stin".into(), window: 0, dialog: 2, page: 1 }]);
}
