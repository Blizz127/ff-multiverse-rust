use super::*;
use crate::ui::draw::DrawnText;
use crate::ui::text::cell_count;

const SIZES: [(u32, u32); 6] = [(320, 240), (640, 360), (1100, 680), (1280, 720), (1920, 1080), (3840, 2160)];

fn running(remaining_ms: u32, total_ms: u32) -> CountdownState {
    CountdownState::Running { remaining_ms, total_ms }
}

fn clock(m: &CountdownMirror, now: f64) -> Option<String> {
    m.view(now).map(|v| v.clock)
}

/// What a draw list says, without the shadows (a shadowed text is recorded twice: the shadow, then the text).
fn said(list: &DrawList) -> Vec<&DrawnText> {
    list.texts().iter().filter(|t| t.color != SHADOW).collect()
}

#[test]
fn a_clock_reads_minutes_and_seconds_rounded_up_and_never_goes_wrong() {
    for (seconds, text) in [
        (600.0, "10:00"),
        (599.5, "10:00"),
        (599.0, "9:59"),
        (61.0, "1:01"),
        (60.0, "1:00"),
        (59.2, "1:00"),
        (1.0, "0:01"),
        (0.4, "0:01"),
        (0.0, "0:00"),
        (-3.0, "0:00"),
        (f64::NAN, "0:00"),
        (f64::INFINITY, "0:00"),
        (f64::NEG_INFINITY, "0:00"),
        (86_400.0, "99:59"),
    ] {
        assert_eq!(clock_text(seconds), text, "{seconds}");
    }
}

#[test]
fn nothing_is_shown_until_the_server_says_so() {
    let m = CountdownMirror::default();
    assert!(m.view(0.0).is_none() && !m.is_running() && m.told() == 0);
    let mut m = m;
    assert!(m.drain_notes().is_empty());
}

#[test]
fn a_running_countdown_counts_down_on_the_clients_clock_and_turns_urgent_for_the_last_minute() {
    let mut m = CountdownMirror::default();
    // told at second 10 of the session that 600 s are left of 600
    m.on_message(10.0, running(600_000, 600_000));
    assert!(m.is_running());
    assert_eq!(clock(&m, 10.0).as_deref(), Some("10:00"));
    assert_eq!(clock(&m, 10.5).as_deref(), Some("10:00"), "rounded up");
    assert_eq!(clock(&m, 11.0).as_deref(), Some("9:59"));
    let v = m.view(40.0).unwrap();
    assert_eq!((v.clock.as_str(), v.tone, v.time_up), ("9:30", Tone::Calm, false));
    assert!((v.fraction - 0.95).abs() < 1e-4, "{}", v.fraction);
    // the last minute: 60 s left is urgent (the deadline is at 610: second 10 and 600 s)
    assert_eq!(m.view(549.9).unwrap().tone, Tone::Calm);
    assert_eq!(m.view(550.0).unwrap().tone, Tone::Urgent);
    // zero stays at 0:00 (urgent) until the server says it is over
    let v = m.view(1_000.0).unwrap();
    assert_eq!((v.clock.as_str(), v.tone, v.fraction, v.time_up), ("0:00", Tone::Urgent, 0.0, false));
    // a later message replaces what is counted (a member that joined late is told what is left)
    m.on_message(1_000.0, running(30_000, 600_000));
    assert_eq!(clock(&m, 1_000.0).as_deref(), Some("0:30"));
    assert_eq!(m.view(1_000.0).unwrap().tone, Tone::Urgent);
    assert_eq!(m.told(), 2);
}

#[test]
fn time_up_is_shown_for_a_few_seconds_and_then_nothing_and_cleared_is_never_shown() {
    let mut m = CountdownMirror::default();
    m.on_message(5.0, running(2_000, 2_000));
    m.on_message(7.0, CountdownState::Over { reason: CountdownEnd::TimeUp });
    assert!(!m.is_running());
    let v = m.view(7.0).unwrap();
    assert_eq!((v.clock.as_str(), v.tone, v.time_up), ("0:00", Tone::Urgent, true));
    assert!(m.view(7.0 + TIME_UP_SHOW_S - 0.1).is_some_and(|v| v.time_up));
    assert!(m.view(7.0 + TIME_UP_SHOW_S).is_none(), "and then nothing");
    // cleared: the story moved past it, nothing is drawn at all
    m.on_message(100.0, running(60_000, 60_000));
    assert!(m.view(100.0).is_some());
    m.on_message(101.0, CountdownState::Over { reason: CountdownEnd::Cleared });
    assert!(m.view(101.0).is_none() && !m.is_running());
}

#[test]
fn the_notes_say_what_the_server_said_in_words_of_our_own() {
    let mut m = CountdownMirror::default();
    m.on_message(0.0, running(600_000, 600_000));
    m.on_message(5.0, CountdownState::Over { reason: CountdownEnd::TimeUp });
    m.on_message(6.0, CountdownState::Over { reason: CountdownEnd::Cleared });
    let notes = m.drain_notes();
    assert_eq!(notes.len(), 3);
    assert!(notes[0].starts_with("running: 600.0 s left of 600.0 s"), "{notes:?}");
    assert!(notes[1].starts_with("over: time is up") && notes[1].contains("nothing else happens"), "{notes:?}");
    assert!(notes[2].starts_with("over: cleared"), "{notes:?}");
    assert!(m.drain_notes().is_empty(), "drained");
    // the notes are bounded
    for i in 0..100 {
        m.on_message(f64::from(i), running(1_000, 1_000));
    }
    assert_eq!(m.drain_notes().len(), MAX_NOTES);
}

#[test]
fn a_remaining_that_exceeds_the_total_does_not_make_a_fraction_above_one() {
    let mut m = CountdownMirror::default();
    m.on_message(0.0, running(900_000, 600_000));
    assert!(m.view(0.0).unwrap().fraction <= 1.0);
    m.on_message(0.0, running(0, 0));
    assert_eq!(m.view(0.0).unwrap().fraction, 0.0);
}

#[test]
fn the_clock_is_drawn_inside_every_window_at_the_top_centre_and_time_up_says_so() {
    let running = View { clock: "10:00".into(), tone: Tone::Calm, fraction: 1.0, time_up: false };
    let up = View { clock: "0:00".into(), tone: Tone::Urgent, fraction: 0.0, time_up: true };
    for (w, h) in SIZES {
        for fixture in [false, true] {
            for view in [&running, &up] {
                let mut list = DrawList::new();
                draw_view(view, w, h, fixture, &mut list);
                let texts = said(&list);
                assert_eq!(texts.len(), if view.time_up { 2 } else { 1 }, "{w}x{h}");
                assert_eq!(texts[0].text, view.clock);
                for t in &texts {
                    let width = cell_count(&t.text) as i32 * 8 * t.scale as i32;
                    assert!(t.x >= 0 && t.y >= 0 && t.x + width <= w as i32 && t.y + 8 * t.scale as i32 <= h as i32, "{w}x{h}: {:?}", t.text);
                }
                // centred (to a pixel or two), and under the banner of a fixture
                let width = cell_count(&texts[0].text) as i32 * 8 * texts[0].scale as i32;
                let middle = texts[0].x + width / 2;
                assert!((middle - w as i32 / 2).abs() <= 2, "{w}x{h}: the clock is at {middle}");
                if fixture {
                    assert!(texts[0].y as f32 >= BANNER_PX, "{w}x{h}: under the banner");
                }
            }
        }
    }
    let mut list = DrawList::new();
    draw_view(&up, 1100, 680, false, &mut list);
    assert_eq!(said(&list)[1].text, TIME_UP_LINE);
}

#[test]
fn drawing_a_mirror_that_shows_nothing_draws_nothing() {
    let mut list = DrawList::new();
    draw(&CountdownMirror::default(), 1100, 680, false, 0.0, &mut list);
    assert!(list.is_empty());
    let mut m = CountdownMirror::default();
    m.on_message(0.0, running(90_000, 90_000));
    draw(&m, 1100, 680, false, 1.0, &mut list);
    assert_eq!(said(&list).iter().map(|t| t.text.as_str()).collect::<Vec<_>>(), ["1:29"]);
}
