//! The battle view's text: each fighter's name and HP numbers, and one line that says whose turn it
//! is (or that the replay diverged, or that this machine has no battle data). Drawn with the
//! client's UI font ([`crate::ui`]) over the flat rectangles of [`super::view`], through the same
//! overlay pass as the HUD.

use super::view::{battle_labels, BattleView, Label, LabelKind, Status, BAND_H};
use crate::ui::draw::DrawList;
use crate::ui::text::{ellipsize, text_width};
use crate::ui::widgets::{ACCENT, DIM, ERROR, INFO, SHADOW, TEXT, WARN};

/// Font scale for a window this wide: the card text is 8 px per character at scale 1.
fn scale_for(vw: u32) -> u32 {
    if vw >= 1000 {
        2
    } else {
        1
    }
}

/// The one line under the status band, and its colour.
pub fn status_line(view: &BattleView) -> (&'static str, [u8; 4]) {
    match view.status {
        Status::Diverged => ("BATTLE DIVERGED FROM THE SERVER", ERROR),
        Status::Unverified => ("No battle data on this computer: waiting for the battle to end", WARN),
        Status::Verified => match view.party.iter().find(|b| b.mine) {
            None => ("Watching the battle", DIM),
            Some(b) if !b.alive => ("Your character is down", ERROR),
            Some(b) if b.ready && view.waiting_for_answer => ("Attacking...", INFO),
            Some(b) if b.ready => ("YOUR TURN: press Space or Enter to attack", ACCENT),
            Some(_) => ("Waiting for your gauge", DIM),
        },
    }
}

/// Fit `label` into its card: the largest scale (at most `max_scale`) at which it fits whole, else
/// scale 1 cut with an ellipsis.
fn fitted(label: &Label, max_scale: u32) -> (u32, String) {
    let width = label.max_width as i32;
    for scale in (1..=max_scale).rev() {
        if text_width(&label.text, scale) <= width {
            return (scale, label.text.clone());
        }
    }
    (1, ellipsize(&label.text, (width / 8).max(1) as usize))
}

/// Draw the text of `view` for a `vw` x `vh` window into `out`.
pub fn draw_battle_text(view: &BattleView, vw: u32, vh: u32, out: &mut DrawList) {
    let scale = scale_for(vw);
    for label in battle_labels(view, vw, vh) {
        let (s, text) = fitted(&label, scale);
        let color = match label.kind {
            LabelKind::Name if label.mine => ACCENT,
            LabelKind::Name => TEXT,
            LabelKind::Hp => DIM,
        };
        out.draw_text_shadowed(label.x as i32, label.y as i32, s, color, SHADOW, &text);
    }
    let (line, color) = status_line(view);
    let width = text_width(line, scale);
    let x = ((vw as i32 - width) / 2).max(2);
    out.draw_text_shadowed(x, BAND_H as i32 + 6, scale, color, SHADOW, line);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::battle::view::Bar;
    use fmv_protocol::BattleId;

    fn bar(name: &str, hp: u32, max: u32, ready: bool, alive: bool, mine: bool) -> Bar {
        Bar { name: name.into(), hp, max_hp: max, gauge: 0, ready, alive, mine }
    }

    fn view(party: Vec<Bar>, enemies: Vec<Bar>, status: Status) -> BattleView {
        BattleView { id: BattleId(1), party, enemies, status, waiting_for_answer: false }
    }

    #[test]
    fn every_fighter_has_a_name_and_hp_text_and_the_turn_is_announced() {
        let v = view(
            vec![bar("Cloud", 120, 300, true, true, true), bar("Barret", 40, 400, false, true, false)],
            vec![bar("Guard Hound", 55, 55, false, true, false)],
            Status::Verified,
        );
        let mut list = DrawList::new();
        draw_battle_text(&v, 1100, 680, &mut list);
        for needle in ["Cloud", "HP 120/300", "Barret", "HP 40/400", "Guard Hound", "HP 55/55", "YOUR TURN"] {
            assert!(list.says(needle), "the battle view says {needle:?}");
        }
        // Our own character's name is in the accent colour, the others' in the plain one.
        // (A shadowed text is drawn twice, the shadow first: look at the one in the text colour.)
        let colour_of = |name: &str| list.texts().iter().find(|t| t.text == name && t.color != SHADOW).map(|t| t.color);
        assert_eq!(colour_of("Cloud"), Some(ACCENT));
        assert_eq!(colour_of("Barret"), Some(TEXT));
    }

    #[test]
    fn the_status_line_follows_the_state_of_the_battle() {
        let mine = |ready: bool, alive: bool| vec![bar("Me", 10, 10, ready, alive, true)];
        let line = |v: &BattleView| status_line(v).0;
        assert!(line(&view(mine(true, true), vec![], Status::Verified)).starts_with("YOUR TURN"));
        assert_eq!(line(&view(mine(false, true), vec![], Status::Verified)), "Waiting for your gauge");
        assert_eq!(line(&view(mine(true, false), vec![], Status::Verified)), "Your character is down");
        assert_eq!(line(&view(vec![bar("Other", 1, 1, true, true, false)], vec![], Status::Verified)), "Watching the battle");
        let mut waiting = view(mine(true, true), vec![], Status::Verified);
        waiting.waiting_for_answer = true;
        assert_eq!(line(&waiting), "Attacking...");
        // A divergence and missing data say so, whatever else is going on.
        assert!(line(&view(mine(true, true), vec![], Status::Diverged)).contains("DIVERGED"));
        let none = BattleView::unverified(BattleId(1));
        assert!(line(&none).contains("No battle data"));
        assert_eq!(status_line(&none).1, WARN);
        assert_eq!(status_line(&view(mine(true, true), vec![], Status::Diverged)).1, ERROR);
    }

    #[test]
    fn long_names_are_cut_to_their_card_and_nothing_is_drawn_outside_the_window() {
        let long = "An Extraordinarily Long Enemy Name That Cannot Fit";
        let v = view(vec![bar(long, 1, 1, false, true, false)], vec![bar(long, 1, 1, false, true, false)], Status::Verified);
        for (w, h) in [(1100u32, 680u32), (640, 480), (320, 240)] {
            let mut list = DrawList::new();
            draw_battle_text(&v, w, h, &mut list);
            assert!(!list.says(long), "{w}x{h}: the full name does not fit in a card");
            assert!(list.says("..") || list.says("\u{2026}"), "{w}x{h}: it is cut with an ellipsis");
            for t in list.texts() {
                assert!(t.x >= 0 && t.y >= 0, "{w}x{h}: {t:?}");
                assert!(t.x < w as i32 && t.y < h as i32, "{w}x{h}: {t:?}");
            }
        }
    }

    #[test]
    fn small_windows_use_the_small_font_and_big_ones_the_big_font() {
        assert_eq!(scale_for(320), 1);
        assert_eq!(scale_for(999), 1);
        assert_eq!(scale_for(1000), 2);
        assert_eq!(scale_for(4000), 2);
        // A card's text keeps its scale when it fits and drops a scale when it does not.
        let label = |text: &str, max_width: f32| Label { x: 0.0, y: 0.0, max_width, text: text.into(), kind: LabelKind::Name, mine: false };
        assert_eq!(fitted(&label("Cloud", 240.0), 2).0, 2);
        assert_eq!(fitted(&label("Cloud", 50.0), 2).0, 1);
        assert!(fitted(&label("Cloud", 3.0), 2).1.chars().count() <= 2);
    }

    #[test]
    fn an_empty_battle_still_says_something() {
        let mut list = DrawList::new();
        draw_battle_text(&view(vec![], vec![], Status::Verified), 800, 600, &mut list);
        assert!(list.says("Watching the battle"));
        // One line (a shadowed text is drawn twice: the shadow, then the text itself).
        assert_eq!(list.texts().iter().filter(|t| t.color != SHADOW).count(), 1);
    }
}
