//! How a fighter's statuses look on the 2D panels: a short code for each (`Psn`, `Slp`, ...) in the status's own colour,
//! in the order the panels list them, and the tint the ATB bar takes while one of them changes its pace.
//!
//! The statuses are the ones the rules play ([`fmv_ff7_battle::status::MODELLED`]); everything about them comes from the
//! replayed engine ([`super::view::Bar::status`]), so the panels never show a status the engine does not have. The
//! codes and colours are this client's own (not game data): three letters fit three cells of the UI font, and the
//! colours follow FF7's habits where it has one (Poison purple, Haste and Regen bright, Stop grey).

use crate::ui::draw::Rgba;
use fmv_ff7_battle::status::{Status, StatusSet};

/// The statuses in the order the panels list them: the ones that stop or hurt first, then the ones that help.
const ORDER: [Status; 13] = [
    Status::Poison,
    Status::Sleep,
    Status::Stop,
    Status::Paralysis,
    Status::Silence,
    Status::Sadness,
    Status::Fury,
    Status::Darkness,
    Status::Slow,
    Status::Haste,
    Status::Regen,
    Status::Barrier,
    Status::MBarrier,
];

/// Cells a code takes, and the cell after it that keeps two codes apart.
pub const CODE_CELLS: usize = 3;
const STRIDE_CELLS: usize = CODE_CELLS + 1;

/// The three letters a panel writes for `status` (`???` for one the rules do not play: never shown).
pub fn code(status: Status) -> &'static str {
    match status {
        Status::Poison => "Psn",
        Status::Sleep => "Slp",
        Status::Stop => "Stp",
        Status::Paralysis => "Par",
        Status::Silence => "Sil",
        Status::Sadness => "Sad",
        Status::Fury => "Fry",
        Status::Darkness => "Drk",
        Status::Slow => "Slw",
        Status::Haste => "Hst",
        Status::Regen => "Rgn",
        Status::Barrier => "Bar",
        Status::MBarrier => "MBr",
        _ => "???",
    }
}

/// The colour of `status`'s code.
pub fn color(status: Status) -> Rgba {
    match status {
        Status::Poison => [196, 128, 236, 255],
        Status::Sleep => [132, 160, 255, 255],
        Status::Stop => [190, 194, 210, 255],
        Status::Paralysis => [255, 214, 64, 255],
        Status::Silence => [168, 172, 190, 255],
        Status::Sadness => [110, 140, 215, 255],
        Status::Fury => [255, 112, 92, 255],
        Status::Darkness => [150, 150, 175, 255],
        Status::Slow => [186, 150, 232, 255],
        Status::Haste => [120, 228, 255, 255],
        Status::Regen => [112, 236, 132, 255],
        Status::Barrier => [130, 200, 255, 255],
        Status::MBarrier => [255, 170, 230, 255],
        _ => [255, 255, 255, 255],
    }
}

/// The statuses of `set` that the panels show, in the panels' order.
pub fn listed(set: StatusSet) -> Vec<Status> {
    ORDER.iter().copied().filter(|s| set.contains(*s)).collect()
}

/// How many whole codes `cells` cells hold: `n` codes take `n * 3 + (n - 1)` cells.
pub fn room(cells: usize) -> usize {
    (cells + 1) / STRIDE_CELLS
}

/// What of `set` fits in `cells` cells: the codes to write, and how many statuses are not among them. When some do not fit
/// and there is room for two codes or more, the last place is taken by a `+N` (the caller writes it, [`more_text`]) and
/// the codes before it are the most important ones; with room for one code that code is written and nothing more.
pub fn fit(set: StatusSet, cells: usize) -> (Vec<Status>, usize) {
    let all = listed(set);
    let room = room(cells);
    if all.len() <= room {
        return (all, 0);
    }
    let shown = if room >= 2 { room - 1 } else { room };
    let kept: Vec<Status> = all.iter().copied().take(shown).collect();
    let left = all.len() - kept.len();
    (kept, left)
}

/// The `+N` that stands for the `left` statuses that did not fit.
pub fn more_text(left: usize) -> String {
    format!("+{}", left.min(99))
}

/// The cell (from the start of the tags) at which the `i`th code starts.
pub fn code_cell(i: usize) -> usize {
    i * STRIDE_CELLS
}

/// The colour the ATB bar of a fighter fills with when its statuses change its pace: grey while it is held (Stop,
/// Paralysis), violet under Slow, cyan under Haste; `None` for the bar's own colours.
pub fn atb_rgb(set: StatusSet) -> Option<[u8; 3]> {
    if set.contains(Status::Stop) || set.contains(Status::Paralysis) {
        Some([140, 142, 154])
    } else if set.contains(Status::Slow) {
        Some([160, 120, 215])
    } else if set.contains(Status::Haste) {
        Some([100, 225, 235])
    } else {
        None
    }
}

/// Whether `set` has a status that hurts or holds (what the flat tokens mark with their border).
pub fn is_afflicted(set: StatusSet) -> bool {
    set.iter().any(|s| s.is_modelled() && !s.is_beneficial())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(list: &[Status]) -> StatusSet {
        list.iter().copied().collect()
    }

    #[test]
    fn every_status_the_rules_play_has_its_own_code_and_colour_and_the_others_have_none() {
        let mut codes = Vec::new();
        let mut colours = Vec::new();
        for status in Status::ALL.into_iter().filter(|s| s.is_modelled()) {
            let c = code(status);
            assert_eq!(c.chars().count(), CODE_CELLS, "{status}");
            assert_ne!(c, "???", "{status}");
            codes.push(c);
            colours.push(color(status));
            assert!(ORDER.contains(&status), "{status} is listed");
        }
        assert_eq!(codes.len(), 13);
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), 13, "no two share a code");
        colours.sort_unstable();
        colours.dedup();
        assert_eq!(colours.len(), 13, "no two share a colour");
        assert!(ORDER.iter().all(|s| s.is_modelled()), "only what the rules play");
        // What the rules do not play is never listed, whatever a set says.
        assert_eq!(listed(StatusSet::from_bits(u32::MAX)).len(), 13);
        assert_eq!(code(Status::Confusion), "???");
    }

    #[test]
    fn the_codes_come_in_the_panels_order_and_fit_by_whole_codes() {
        let all = set(&[Status::Haste, Status::Poison, Status::Silence, Status::Regen]);
        assert_eq!(listed(all), [Status::Poison, Status::Silence, Status::Haste, Status::Regen], "what hurts first");
        // Three cells hold one code, seven two, eleven three: a cell between them. When they do not all fit, the last place
        // says how many are not shown.
        for (cells, shown, left) in [(0, 0, 4), (2, 0, 4), (3, 1, 3), (6, 1, 3), (7, 1, 3), (11, 2, 2), (15, 4, 0), (40, 4, 0)] {
            let (got, more) = fit(all, cells);
            assert_eq!((got.len(), more), (shown, left), "{cells} cells");
        }
        assert_eq!(fit(all, 7).0, [Status::Poison], "the most important ones, then the +N: {}", more_text(3));
        assert_eq!(more_text(3), "+3");
        assert_eq!(fit(StatusSet::EMPTY, 8), (Vec::new(), 0));
        assert_eq!((room(0), room(3), room(7), room(11)), (0, 1, 2, 3));
        assert_eq!((code_cell(0), code_cell(1), code_cell(2)), (0, 4, 8));
    }

    #[test]
    fn the_atb_bar_changes_colour_only_for_the_statuses_that_change_its_pace_and_a_hold_wins() {
        assert_eq!(atb_rgb(StatusSet::EMPTY), None);
        assert_eq!(atb_rgb(set(&[Status::Poison, Status::Silence, Status::Regen])), None);
        let (haste, slow, held) = (atb_rgb(set(&[Status::Haste])), atb_rgb(set(&[Status::Slow])), atb_rgb(set(&[Status::Stop])));
        assert!(haste.is_some() && slow.is_some() && held.is_some());
        assert!(haste != slow && slow != held && haste != held);
        assert_eq!(atb_rgb(set(&[Status::Paralysis, Status::Haste])), held, "a hold is all that shows");
        assert_eq!(atb_rgb(set(&[Status::Slow, Status::Stop])), held);
    }

    #[test]
    fn the_flat_tokens_mark_what_hurts_or_holds_and_not_what_helps() {
        assert!(is_afflicted(set(&[Status::Poison])) && is_afflicted(set(&[Status::Haste, Status::Silence])));
        assert!(!is_afflicted(set(&[Status::Haste, Status::Regen, Status::Barrier])) && !is_afflicted(StatusSet::EMPTY));
    }
}
