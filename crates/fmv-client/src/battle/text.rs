//! The battle overlay as a whole, and its text: the status band and the one line under it (whose
//! turn it is, what a key does, that the replay diverged or that this machine has no battle data),
//! then the windows, the banner, the victory or defeat panel, the target pointer and the floating
//! numbers ([`super::panels`]). Drawn with the client's UI font ([`crate::ui`]) through the same
//! overlay pass as the HUD, over the flat tokens of [`super::view`] or over a 3D scene.
//!
//! * [`draw_battle_overlay`] is the whole overlay for a scene that knows where its fighters are
//!   ([`Anchors`]); it draws no fighters of its own.
//! * [`draw_battle_text`] is the same for the flat tokens: it takes their anchors from
//!   [`flat_anchors`] and adds the names under them.

use super::menu::{Level, MenuView};
use super::panels::{draw_banner, draw_finale, draw_pointer, draw_popups, draw_windows, layout, DISABLED};
use super::view::{cards, flat_anchors, Anchors, BattleView, Status, BAND_H};
use crate::ui::draw::{DrawList, Rgba};
use crate::ui::text::{ellipsize, text_width};
use crate::ui::widgets::{Canvas, ACCENT, DIM, ERROR, INFO, SHADOW, TEXT, WARN};
use fmv_ff7_battle::rules::Outcome;

/// The line under the status band for an open menu: why the entry under the cursor cannot be chosen,
/// else what the keys do at this level.
fn menu_line(menu: &MenuView) -> (&'static str, Rgba) {
    if let Some(why) = menu.hint {
        return (why, WARN);
    }
    let line = match menu.level {
        Level::Commands => "YOUR TURN   Arrows: move   Enter: select",
        Level::Magic | Level::Items => "Enter: choose   Backspace: back",
        Level::Target => "Arrows: pick a target   Enter: confirm   Backspace: back",
    };
    (line, ACCENT)
}

/// The one line under the status band, and its colour.
pub fn status_line(view: &BattleView) -> (&'static str, Rgba) {
    match view.status {
        Status::Diverged => ("BATTLE DIVERGED FROM THE SERVER", ERROR),
        Status::Unverified => ("No battle data on this computer: waiting for the battle to end", WARN),
        Status::Verified => {
            if let Some(finale) = &view.finale {
                return if finale.outcome == Outcome::Victory { ("Victory!", ACCENT) } else { ("Defeat", ERROR) };
            }
            match view.party.iter().find(|b| b.mine) {
                None => ("Watching the battle", DIM),
                Some(b) if !b.alive => ("Your character is down", ERROR),
                Some(b) if b.ready && view.waiting_for_answer => ("Waiting for the server...", INFO),
                Some(b) if b.ready => view.menu.as_ref().map_or(("YOUR TURN", ACCENT), menu_line),
                Some(_) => ("Waiting for your gauge", DIM),
            }
        }
    }
}

/// The names of the flat tokens, centred under them.
fn draw_token_labels(view: &BattleView, vw: u32, vh: u32, out: &mut DrawList) {
    let s = layout(vw, vh, view.party.len()).s;
    let scale = u32::try_from(s).unwrap_or(1);
    for card in cards(view, vw, vh) {
        let cells = ((card.rect.width() as i32 + 8 * s * 2) / (8 * s)).max(1) as usize;
        let name = ellipsize(&card.bar.name, cells);
        let w = text_width(&name, scale);
        let color = if !card.bar.alive {
            DISABLED
        } else if card.bar.mine {
            ACCENT
        } else {
            TEXT
        };
        let x = (((card.rect.x0 + card.rect.x1) * 0.5) as i32 - w / 2).clamp(0, (vw as i32 - w).max(0));
        out.draw_text_shadowed(x, card.rect.y1 as i32 + 2 * s, scale, color, SHADOW, &name);
    }
}

/// The whole overlay of `view` for a `vw` x `vh` window, over a scene that places its fighters at
/// `anchors`: the status band and line, the bottom windows, the banner, the victory or defeat
/// panel, the pointer and the floating numbers. It draws the status band itself (in the colour of
/// the replay's standing), so a scene that draws none still has it.
pub fn draw_battle_overlay(view: &BattleView, anchors: &Anchors, vw: u32, vh: u32, out: &mut DrawList) {
    let l = layout(vw, vh, view.party.len());
    let band = view.status.band_rgb();
    out.fill_rect(0, 0, i32::try_from(vw).unwrap_or(i32::MAX), BAND_H as i32, [band[0], band[1], band[2], 255]);
    {
        let mut c = Canvas { out, s: l.s };
        let (line, color) = status_line(view);
        c.centered(0, l.cw, l.status_y, color, line);
        draw_windows(&mut c, &l, view);
        if let Some(banner) = &view.banner {
            draw_banner(&mut c, &l, banner);
        }
        if let Some(finale) = &view.finale {
            draw_finale(&mut c, &l, finale);
        }
    }
    draw_pointer(out, &l, view, anchors, vw);
    draw_popups(out, &l, view, anchors, vw, vh);
}

/// The overlay over the flat tokens of [`super::view::battle_vertices`]: [`draw_battle_overlay`]
/// with the tokens' own anchors, and each token's name under it.
pub fn draw_battle_text(view: &BattleView, vw: u32, vh: u32, out: &mut DrawList) {
    draw_token_labels(view, vw, vh, out);
    draw_battle_overlay(view, &flat_anchors(view, vw, vh), vw, vh, out);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::battle::menu::{BattleMenu, BattleMenuModel, Ctx, Fighter, MenuKey, MenuSpell, TargetRule};
    use crate::battle::panels::{Finale, RewardLine};
    use crate::battle::popups::{Popup, PopupKind};
    use crate::battle::view::Bar;
    use crate::ui::text::text_height;
    use fmv_ff7_battle::rules::ActorId;
    use fmv_protocol::BattleId;

    fn bar(actor: u8, name: &str, hp: u32, max: u32, ready: bool, alive: bool, mine: bool) -> Bar {
        Bar { actor: ActorId(actor), name: name.into(), hp, max_hp: max, mp: 10, max_mp: 20, ready, alive, mine, ..Bar::default() }
    }

    fn view(party: Vec<Bar>, enemies: Vec<Bar>, status: Status) -> BattleView {
        BattleView { id: BattleId(1), party, enemies, status, ..BattleView::default() }
    }

    fn open_menu(v: &mut BattleView, keys: &[MenuKey]) {
        let model = BattleMenuModel { spells: vec![MenuSpell::new(1, "Fire", 4, TargetRule::Enemies), MenuSpell::new(2, "Meteor", 99, TargetRule::Enemies)], items: vec![], defend: false };
        let alive = |bars: &[Bar]| bars.iter().map(|b| Fighter { id: b.actor, alive: b.alive }).collect::<Vec<_>>();
        let (a, e) = (alive(&v.party), alive(&v.enemies));
        let ctx = Ctx { model: &model, allies: &a, enemies: &e, mp: 10 };
        let mut menu = BattleMenu::default();
        for k in keys {
            menu.press(*k, &ctx);
        }
        v.menu = Some(menu.view(&ctx));
    }

    #[test]
    fn every_fighter_is_named_and_the_turn_is_announced() {
        let mut v = view(
            vec![bar(0, "Cloud", 120, 300, true, true, true), bar(1, "Barret", 40, 400, false, true, false)],
            vec![bar(2, "Guard Hound", 55, 55, false, true, false)],
            Status::Verified,
        );
        open_menu(&mut v, &[]);
        let mut list = DrawList::new();
        draw_battle_text(&v, 1100, 680, &mut list);
        for needle in ["Cloud", "120/300", "Barret", " 40/400", "Guard Hound", "YOUR TURN", "Attack"] {
            assert!(list.says(needle), "the battle view says {needle:?}");
        }
        // Our own character's name is in the accent colour, the others' in the plain one. (A shadowed text
        // is drawn twice, the shadow first: look at the one in the text colour.)
        let colour_of = |name: &str| list.texts().iter().find(|t| t.text == name && t.color != SHADOW).map(|t| t.color);
        assert_eq!(colour_of("Cloud"), Some(ACCENT));
        assert_eq!(colour_of("Barret"), Some(TEXT));
    }

    #[test]
    fn the_status_line_follows_the_state_of_the_battle() {
        let mine = |ready: bool, alive: bool| vec![bar(0, "Me", 10, 10, ready, alive, true)];
        let line = |v: &BattleView| status_line(v).0;
        assert_eq!(line(&view(mine(true, true), vec![], Status::Verified)), "YOUR TURN", "ready, but no menu data given");
        assert_eq!(line(&view(mine(false, true), vec![], Status::Verified)), "Waiting for your gauge");
        assert_eq!(line(&view(mine(true, false), vec![], Status::Verified)), "Your character is down");
        assert_eq!(line(&view(vec![bar(0, "Other", 1, 1, true, true, false)], vec![], Status::Verified)), "Watching the battle");
        let mut waiting = view(mine(true, true), vec![], Status::Verified);
        waiting.waiting_for_answer = true;
        assert_eq!(line(&waiting), "Waiting for the server...");
        // A divergence and missing data say so, whatever else is going on.
        assert!(line(&view(mine(true, true), vec![], Status::Diverged)).contains("DIVERGED"));
        let none = BattleView::unverified(BattleId(1));
        assert!(line(&none).contains("No battle data"));
        assert_eq!(status_line(&none).1, WARN);
        assert_eq!(status_line(&view(mine(true, true), vec![], Status::Diverged)).1, ERROR);
    }

    #[test]
    fn the_status_line_tells_what_the_keys_do_at_each_level_and_why_an_entry_is_greyed_out() {
        let mut v = view(vec![bar(0, "Me", 10, 10, true, true, true)], vec![bar(1, "Foe", 5, 5, false, true, false)], Status::Verified);
        open_menu(&mut v, &[]);
        assert!(status_line(&v).0.starts_with("YOUR TURN"));
        open_menu(&mut v, &[MenuKey::Down, MenuKey::Confirm]);
        assert_eq!(status_line(&v).0, "Enter: choose   Backspace: back");
        // The cursor on a spell that costs too much: the reason replaces the hint, in the warning colour.
        open_menu(&mut v, &[MenuKey::Down, MenuKey::Confirm, MenuKey::Down]);
        assert_eq!(status_line(&v), ("Not enough MP", WARN));
        open_menu(&mut v, &[MenuKey::Confirm]);
        assert!(status_line(&v).0.contains("pick a target"));
        // A greyed-out command says why while the cursor is on it.
        open_menu(&mut v, &[MenuKey::Up]);
        assert_eq!(status_line(&v).0, "Not available yet");
        // Waiting for the server beats the menu (it is closed then anyway).
        v.menu = None;
        v.waiting_for_answer = true;
        assert_eq!(status_line(&v).0, "Waiting for the server...");
    }

    #[test]
    fn the_end_of_a_battle_is_announced_in_the_line_and_a_divergence_still_wins() {
        let mut v = view(vec![bar(0, "Me", 10, 10, false, true, true)], vec![], Status::Verified);
        v.finale = Some(Finale { outcome: Outcome::Victory, reward: Some(RewardLine::default()), age_ms: 0, dismissible: false });
        assert_eq!(status_line(&v).0, "Victory!");
        v.finale = Some(Finale { outcome: Outcome::Defeat, reward: None, age_ms: 0, dismissible: false });
        assert_eq!(status_line(&v), ("Defeat", ERROR));
        v.status = Status::Diverged;
        assert!(status_line(&v).0.contains("DIVERGED"));
    }

    #[test]
    fn the_overlay_draws_the_status_band_itself_in_the_colour_of_the_replays_standing() {
        for (status, rgb) in [(Status::Verified, [40, 150, 80]), (Status::Diverged, [220, 30, 30]), (Status::Unverified, [120, 120, 130])] {
            let mut list = DrawList::new();
            draw_battle_overlay(&view(vec![], vec![], status), &Anchors::default(), 1100, 680, &mut list);
            let band = &list.quads()[0];
            assert_eq!((band.x, band.y, band.w, band.h), (0, 0, 1100, BAND_H as i32), "{status:?}: a strip across the top");
            assert_eq!(band.color, [rgb[0], rgb[1], rgb[2], 255], "{status:?}");
        }
    }

    #[test]
    fn long_names_are_cut_and_nothing_is_drawn_outside_the_window() {
        let long = "An Extraordinarily Long Enemy Name That Cannot Fit";
        let v = view(vec![bar(0, long, 1, 1, false, true, false)], vec![bar(1, long, 1, 1, false, true, false)], Status::Verified);
        for (w, h) in [(1100u32, 680u32), (640, 480), (320, 240)] {
            let mut list = DrawList::new();
            draw_battle_text(&v, w, h, &mut list);
            assert!(!list.says(long), "{w}x{h}: the full name does not fit in a window");
            assert!(list.says("..."), "{w}x{h}: it is cut with an ellipsis");
            for t in list.texts() {
                assert!(t.x >= 0 && t.y >= 0, "{w}x{h}: {t:?}");
                assert!(t.x + text_width(&t.text, t.scale) <= w as i32 && t.y + text_height(t.scale) <= h as i32, "{w}x{h}: {t:?}");
            }
        }
    }

    #[test]
    fn an_empty_battle_still_says_something() {
        let mut list = DrawList::new();
        draw_battle_text(&view(vec![], vec![], Status::Verified), 800, 600, &mut list);
        assert!(list.says("Watching the battle"));
        // One line (a shadowed text is drawn twice: the shadow, then the text itself).
        assert_eq!(list.texts().iter().filter(|t| t.color != SHADOW).count(), 1);
    }

    #[test]
    fn flat_tokens_get_their_names_and_a_scene_with_anchors_gets_none() {
        let v = view(vec![bar(0, "Cloud", 10, 10, false, true, true)], vec![bar(1, "Guard", 5, 5, false, true, false)], Status::Verified);
        let mut flat = DrawList::new();
        draw_battle_text(&v, 1100, 680, &mut flat);
        let under_tokens = |list: &DrawList, name: &str| list.texts().iter().filter(|t| t.text == name && t.color != SHADOW).count();
        // Each name is in its window and under its token.
        assert_eq!((under_tokens(&flat, "Cloud"), under_tokens(&flat, "Guard")), (2, 2));
        let mut scene = DrawList::new();
        let anchors = flat_anchors(&v, 1100, 680);
        draw_battle_overlay(&v, &anchors, 1100, 680, &mut scene);
        assert_eq!((under_tokens(&scene, "Cloud"), under_tokens(&scene, "Guard")), (1, 1), "only the windows");
    }

    #[test]
    fn popups_and_the_pointer_follow_the_anchors_a_scene_gives() {
        let mut v = view(vec![bar(0, "Me", 10, 10, true, true, true)], vec![bar(1, "Foe", 5, 5, false, true, false)], Status::Verified);
        open_menu(&mut v, &[MenuKey::Confirm]);
        v.popups = vec![Popup { target: ActorId(1), kind: PopupKind::Damage { amount: 321, critical: false }, age_ms: 350, slot: 0 }];
        let anchors = Anchors { party: vec![Some((900.0, 300.0))], enemies: vec![Some((200.0, 250.0))] };
        let mut list = DrawList::new();
        draw_battle_overlay(&v, &anchors, 1100, 680, &mut list);
        let t = list.texts().iter().find(|t| t.text == "321" && t.color != SHADOW).expect("the number");
        assert!((t.x + text_width("321", t.scale) / 2 - 200).abs() <= 2, "over the enemy's anchor, not the flat token: {t:?}");
        assert!(list.quads().iter().any(|q| q.color == ACCENT && (q.x + q.w / 2 - 200).abs() <= 2), "the pointer is over the same anchor");
        // The same view with no anchor for the enemy has neither.
        let mut none = DrawList::new();
        draw_battle_overlay(&v, &Anchors::default(), 1100, 680, &mut none);
        assert!(!none.says("321"));
    }
}
