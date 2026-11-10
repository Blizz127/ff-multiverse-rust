//! The in-game overlay: player name labels over their markers, the party list, the area name, the
//! local player's character progress, the connection status and, for synthetic content, the FIXTURE
//! label.
//!
//! [`draw_hud`] is a pure function of a [`HudInput`] (names, view-space positions, counts) and the
//! window size; [`gather`] reads the live session into one. [`draw_hud_with`] adds the one line about
//! the local player's character (`Wedge (Gunner)  Lv 7  EXP 626/949  Gil 20`, see [`progress_line`]) under the
//! area name; it is a separate argument, not a field of [`HudInput`], so that everything that builds
//! or draws a plain `HudInput` is unchanged. Text is drawn at an integer scale on translucent plates
//! so it stays readable over any background picture.

use super::draw::{DrawList, Rgba};
use super::widgets::{ACCENT, DIM, SHADOW, TEXT};
use super::text::{ellipsize, text_height, text_width};
use crate::backdrop::FieldView;
use crate::interp::render_time;
use crate::net::clean;
use crate::render::{view_to_window, BANNER_PX};
use crate::session::{Session, SessionInfo};
use crate::state::ClientState;
use fmv_adapter_api::Bounds;
use fmv_core::{AreaId, EntityId, Pos};
use std::time::Duration;

/// What the HUD says for synthetic content. Shown whenever the area is a fixture.
pub const FIXTURE_LABEL: &str = "FIXTURE (synthetic, not FF7)";

const PLATE: Rgba = [6, 8, 24, 175];
const GOOD: Rgba = [124, 232, 148, 255];
const PARTY_RING: Rgba = [255, 208, 64, 255];
const OTHER: Rgba = [214, 222, 240, 255];
/// Non-player characters: a dimmer, cooler tone than any player's name.
const NPC_TONE: Rgba = [150, 184, 208, 255];
const FIXTURE_BG: Rgba = [255, 196, 0, 255];
const FIXTURE_INK: Rgba = [24, 24, 24, 255];
/// Most characters of a name shown in a corner list or a label.
const NAME_CELLS: usize = 20;
/// A label sits this many pixels above the marker's centre (clear of the party ring).
const LABEL_LIFT_PX: i32 = 23;
/// How far out of the window (pixels, at scale 1) a name label's anchor may be and still get its label: a head just
/// over an edge keeps its name; somebody the field camera has out of the window does not.
const OFF_WINDOW_PX: f32 = 32.0;

/// The integer text scale of the HUD for a `w` x `h` window (1 up to 960 x 540, then 2, ...).
pub fn hud_scale(w: u32, h: u32) -> u32 {
    (w / 960).min(h / 540).clamp(1, 4)
}

/// The area name as the HUD (and the loading screen) show it: the game and the field, as in
/// `FF7 md1stin`. An area flagged as a fixture always says FIXTURE.
pub fn area_label(area: &AreaId, fixture: bool) -> String {
    let label = format!("{} {}", area.game.key().to_uppercase(), clean(&area.key));
    if fixture && !area.game.is_fixture() {
        format!("FIXTURE {label}")
    } else {
        label
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelKind {
    /// The local player.
    Own,
    /// A member of the local player's party.
    Party,
    Other,
    /// A non-player character of the area (its name, over its marker).
    Npc,
}

/// A player's name, at the player's marker (view space, as the renderer gets it).
#[derive(Debug, Clone, PartialEq)]
pub struct Label {
    pub x: f32,
    pub y: f32,
    pub text: String,
    pub kind: LabelKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartyRow {
    pub name: String,
    pub leader: bool,
    pub you: bool,
}

/// Everything the HUD says.
#[derive(Debug, Clone)]
pub struct HudInput {
    /// The view bounds the scene is fitted to (see [`view_to_window`]).
    pub bounds: Bounds,
    pub labels: Vec<Label>,
    pub area: String,
    pub fixture: bool,
    /// Players in the area, the local one included.
    pub players: usize,
    /// The last round-trip time; `None` before the first reply.
    pub rtt: Option<Duration>,
    pub party: Vec<PartyRow>,
    /// A pending party invite addressed to the local player: who it is from.
    pub invite_from: Option<String>,
}

/// Read the live session into a [`HudInput`].
#[allow(dead_code)] // the window gathers with its own label anchors (`gather_anchored`)
pub fn gather(session: &Session, view: &FieldView) -> HudInput {
    gather_from(&session.state, &session.info, view, render_time(session.now()))
}

/// [`gather`] over the pieces it reads, so it can be tested without a connection.
#[allow(dead_code)]
pub fn gather_from(state: &ClientState, info: &SessionInfo, view: &FieldView, render_t: f64) -> HudInput {
    gather_anchored(state, info, view, render_t, &|_, at| view.to_view(at))
}

/// Whose name a label is: what [`gather_anchored`] asks its `anchor` about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subject {
    /// The local player.
    Own,
    /// Another player.
    Remote(EntityId),
    /// A non-player character of the area (its index in the area's list, `NpcView::id`).
    Npc(u32),
}

/// [`gather_from`] with the label's anchor chosen by `anchor` (given whose name it is and where
/// that one stands; it answers the view-space point the name goes over, or `None` if it cannot be
/// drawn). The marker view anchors at the marker; the model view at the top of the head (see
/// `crate::avatar`).
pub fn gather_anchored(
    state: &ClientState,
    info: &SessionInfo,
    view: &FieldView,
    render_t: f64,
    anchor: &dyn Fn(Subject, Pos) -> Option<Pos>,
) -> HudInput {
    let mut labels = Vec::new();
    // The area's NPCs (static: they are named where they stand; one that cannot be drawn has no
    // label either).
    for npc in state.npc_views() {
        if let Some(p) = anchor(Subject::Npc(npc.id), npc.pos) {
            labels.push(Label { x: p.x, y: p.y, text: npc.name.clone(), kind: LabelKind::Npc });
        }
    }
    for remote in state.remote_views(render_t) {
        // One that cannot be drawn (behind the camera) has no marker, so no label either.
        if let Some(p) = anchor(Subject::Remote(remote.id), remote.pose.pos) {
            let kind = if state.party.is_party_entity(remote.id) { LabelKind::Party } else { LabelKind::Other };
            labels.push(Label { x: p.x, y: p.y, text: remote.name.clone(), kind });
        }
    }
    if let Some(p) = anchor(Subject::Own, state.own.pos()) {
        labels.push(Label { x: p.x, y: p.y, text: info.name.clone(), kind: LabelKind::Own });
    }
    HudInput {
        bounds: view.bounds,
        labels,
        area: area_label(&info.area_id, info.fixture),
        fixture: info.fixture,
        players: state.remote_count() + 1,
        rtt: state.last_rtt(),
        party: state
            .party
            .party_members()
            .into_iter()
            .map(|m| PartyRow { name: m.name, leader: m.leader, you: m.you })
            .collect(),
        invite_from: state.party.newest_invite_to_me().map(|i| i.from_name),
    }
}

/// An axis-aligned rectangle (x0, y0, x1, y1).
type Rect = (i32, i32, i32, i32);

fn overlaps(a: Rect, b: Rect) -> bool {
    a.0 < b.2 && b.0 < a.2 && a.1 < b.3 && b.1 < a.3
}

/// Space between a plate's edge and its text.
fn pad(s: i32) -> i32 {
    4 * s
}

/// Distance between the tops of two lines of text.
fn line_h(s: i32) -> i32 {
    text_height(s as u32) + 3 * s
}

/// The height of a plate with `lines` lines of text.
fn block_h(lines: usize, s: i32) -> i32 {
    lines as i32 * line_h(s) - 3 * s + 2 * pad(s)
}

/// A text block: lines on one translucent plate. Returns the plate's rectangle.
fn block(out: &mut DrawList, x: i32, y: i32, s: i32, lines: &[(Rgba, String)]) -> Rect {
    let width = lines.iter().map(|(_, t)| text_width(t, s as u32)).max().unwrap_or(0);
    let rect = (x, y, x + width + 2 * pad(s), y + block_h(lines.len(), s));
    out.fill_rect(rect.0, rect.1, rect.2 - rect.0, rect.3 - rect.1, PLATE);
    for (i, (color, text)) in lines.iter().enumerate() {
        out.draw_text_shadowed(x + pad(s), y + pad(s) + i as i32 * line_h(s), s as u32, *color, SHADOW, text);
    }
    rect
}

/// What the HUD says about the local player's character (see `crate::progress`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgressLine {
    /// `Wedge (Gunner)  Lv 7  EXP 626/949  Gil 20`.
    pub text: String,
    /// The server reported a level gained a few seconds ago: say so on a line of its own.
    pub level_up: bool,
}

/// The line about the local player's character as of local time `now`, or `None` while the server has
/// not said anything about it (a server without battles never does).
pub fn progress_line(state: &ClientState, now: f64) -> Option<ProgressLine> {
    Some(ProgressLine { text: state.progress.hud_text()?, level_up: state.progress.level_up_shown(now) })
}

/// Draw the HUD for a `w` x `h` window, without a line about the local player's character.
#[allow(dead_code)] // the window draws with its character line (`draw_hud_with`); the shots and tests without
pub fn draw_hud(input: &HudInput, w: u32, h: u32, out: &mut DrawList) {
    draw_hud_with(input, None, w, h, out);
}

/// [`draw_hud`] plus, when there is one, the line about the local player's character: under the area
/// name, on the same plate (so the FIXTURE label, which keeps clear of that plate, keeps clear of it
/// too).
pub fn draw_hud_with(input: &HudInput, progress: Option<&ProgressLine>, w: u32, h: u32, out: &mut DrawList) {
    let (wi, hi) = (w as i32, h as i32);
    let s = hud_scale(w, h) as i32;
    let m = 6 * s;
    // The most characters a line of a plate may have in this window; longer ones are cut.
    let fit = |text: &str| ellipsize(text, ((wi - 2 * m - 2 * pad(s)) / (8 * s)).max(4) as usize);
    // Below the hazard banner that fixture areas wear across the top.
    let top = if input.fixture { BANNER_PX as i32 } else { 0 };

    draw_labels(input, w, h, s, top, out);

    // Top left: where we are, and how our character stands.
    let mut place = vec![(TEXT, fit(&ellipsize(&input.area, 40)))];
    if let Some(p) = progress {
        place.push((OTHER, fit(&ellipsize(&p.text, 60))));
        if p.level_up {
            place.push((PARTY_RING, "LEVEL UP!".to_string()));
        }
    }
    let area = block(out, m, top + m, s, &place);

    // The fixture label, centred under the banner (below the area name if they would collide).
    if input.fixture {
        let tw = text_width(FIXTURE_LABEL, s as u32);
        let (w, h) = (tw + 2 * pad(s), block_h(1, s));
        let mut x = ((wi - w) / 2).max(2);
        let mut y = top + 3 * s;
        if overlaps((x, y, x + w, y + h), area) {
            x = m;
            y = area.3 + 3 * s;
        }
        out.fill_rect(x, y, w, h, FIXTURE_BG);
        out.draw_text(x + pad(s), y + pad(s), s as u32, FIXTURE_INK, FIXTURE_LABEL);
    }

    // Top right: the party.
    if !input.party.is_empty() {
        let mut lines = vec![(ACCENT, format!("PARTY ({})", input.party.len()))];
        for member in &input.party {
            let mark = if member.leader { '*' } else { ' ' };
            let you = if member.you { " (you)" } else { "" };
            let tone = if member.you { TEXT } else { OTHER };
            lines.push((tone, fit(&format!("{mark} {}{you}", ellipsize(&member.name, NAME_CELLS)))));
        }
        let width = lines.iter().map(|(_, t)| text_width(t, s as u32)).max().unwrap_or(0) + 2 * pad(s);
        block(out, wi - m - width, top + m, s, &lines);
    }

    // Bottom left: the connection.
    let (status_tone, status_text) = match input.rtt {
        Some(_) => (GOOD, "ONLINE"),
        None => (ACCENT, "ONLINE (waiting for the first reply)"),
    };
    let rtt = input.rtt.map_or_else(|| "-".to_string(), |d| format!("{} ms", d.as_millis()));
    let status_lines = vec![(status_tone, status_text.to_string()), (DIM, format!("players {}   rtt {rtt}", input.players))];
    let status = block(out, m, hi - m - block_h(2, s), s, &status_lines);

    // Bottom right: the keys, when there is room beside the status. Shift (walk), F1 (the
    // walkmesh) and M (mute; - and = change the volume) only mean something over a real field's
    // picture and its music, not in a fixture.
    let hint = if input.fixture {
        "WASD move   P invite   Y accept   L leave   Esc x2 quit"
    } else {
        "WASD move   Shift walk   F1 mesh   P invite   Y accept   L leave   M mute   Esc x2 quit"
    };
    let hint_w = text_width(hint, s as u32) + 2 * pad(s);
    if status.2 + hint_w + 2 * m < wi {
        block(out, wi - m - hint_w, hi - m - block_h(1, s), s, &[(DIM, hint.to_string())]);
    }

    // A pending invite is the one thing that needs an answer: it gets its own line.
    if let Some(from) = &input.invite_from {
        let text = fit(&format!("INVITE from {}: press Y to join", ellipsize(from, NAME_CELLS)));
        let tw = text_width(&text, s as u32) + 2 * pad(s);
        let x = ((wi - tw) / 2).max(2);
        block(out, x, status.1 - block_h(1, s) - 2 * s, s, &[(ACCENT, text)]);
    }
}

/// Name labels above the markers, kept inside the window.
fn draw_labels(input: &HudInput, w: u32, h: u32, s: i32, top: i32, out: &mut DrawList) {
    let (wi, hi) = (w as i32, h as i32);
    let cell = text_height(s as u32);
    // The own label last, so it is never hidden under another player's.
    let own = input.labels.iter().filter(|l| l.kind == LabelKind::Own);
    let rest = input.labels.iter().filter(|l| l.kind != LabelKind::Own);
    for label in rest.chain(own) {
        if !(label.x.is_finite() && label.y.is_finite()) {
            continue;
        }
        let [px, py] = view_to_window(&input.bounds, w, h, label.x, label.y);
        if !(px.is_finite() && py.is_finite()) {
            continue;
        }
        // Somebody the field camera has out of the window gets no label (a near one, a head just over an edge, is kept
        // inside the window like every label).
        let off = OFF_WINDOW_PX * s as f32;
        if px < -off || px > w as f32 + off || py < -off || py > h as f32 + off {
            continue;
        }
        let text = ellipsize(&label.text, NAME_CELLS);
        if text.is_empty() {
            continue;
        }
        let tw = text_width(&text, s as u32);
        let x = (px.round() as i32 - tw / 2).clamp(2 * s, (wi - tw - 2 * s).max(2 * s));
        let y = (py.round() as i32 - LABEL_LIFT_PX - cell).clamp(top + 2 * s, (hi - cell - 2 * s).max(top + 2 * s));
        let tone = match label.kind {
            LabelKind::Own => [255, 255, 255, 255],
            LabelKind::Party => PARTY_RING,
            LabelKind::Other => OTHER,
            LabelKind::Npc => NPC_TONE,
        };
        out.fill_rect(x - 3 * s, y - 2 * s, tw + 6 * s, cell + 4 * s, [0, 0, 0, 150]);
        out.draw_text_shadowed(x, y, s as u32, tone, SHADOW, &text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backdrop::FieldView;
    use crate::ui::draw::DrawnText;
    use crate::ui::text::cell_count;
    use fmv_adapter_api::GameAdapter;
    use fmv_core::{AccountId, AnimState, AvatarAppearance, EntityId, EntitySnapshot, GameId, Pos};
    use fmv_fixture::FixtureAdapter;
    use fmv_protocol::{InviteId, PartyEvent, PartyId, PartyName, ServerMsg};

    const SIZES: [(u32, u32); 6] = [(320, 240), (640, 360), (1100, 680), (1280, 720), (1920, 1080), (3840, 2160)];

    fn bounds() -> Bounds {
        Bounds { min: Pos::new(-400.0, -300.0, 0.0), max: Pos::new(400.0, 300.0, 0.0) }
    }

    fn input() -> HudInput {
        HudInput {
            bounds: bounds(),
            labels: vec![],
            area: "FF7 md1stin".into(),
            fixture: false,
            players: 1,
            rtt: None,
            party: vec![],
            invite_from: None,
        }
    }

    fn draw(input: &HudInput, w: u32, h: u32) -> DrawList {
        let mut list = DrawList::new();
        draw_hud(input, w, h, &mut list);
        list
    }

    fn rect_of(t: &DrawnText) -> Rect {
        let w = cell_count(&t.text) as i32 * 8 * t.scale as i32;
        (t.x, t.y, t.x + w, t.y + 8 * t.scale as i32)
    }

    fn assert_inside(list: &DrawList, w: u32, h: u32, what: &str) {
        for t in list.texts() {
            let (x0, y0, x1, y1) = rect_of(t);
            assert!(x0 >= 0 && y0 >= 0 && x1 <= w as i32 && y1 <= h as i32, "{what} at {w}x{h}: {:?} ({x0},{y0})-({x1},{y1})", t.text);
        }
    }

    fn label(x: f32, y: f32, name: &str, kind: LabelKind) -> Label {
        Label { x, y, text: name.into(), kind }
    }

    // ---- the pure parts --------------------------------------------------------------------------

    #[test]
    fn the_area_is_named_by_game_and_field_and_a_fixture_always_says_so() {
        let ff7 = AreaId { game: GameId::Ff7, key: "md1stin".into() };
        assert_eq!(area_label(&ff7, false), "FF7 md1stin");
        let fixture = AreaId { game: GameId::Fixture, key: "plaza".into() };
        assert_eq!(area_label(&fixture, true), "FIXTURE plaza");
        // A real game's id on an area that is flagged synthetic is still labelled.
        assert_eq!(area_label(&ff7, true), "FIXTURE FF7 md1stin");
        // Text from the network is cleaned.
        let hostile = AreaId { game: GameId::Ff7, key: "a\u{1b}[31mb".into() };
        assert_eq!(area_label(&hostile, false), "FF7 a[31mb");
    }

    #[test]
    fn the_hud_scale_follows_the_window() {
        assert_eq!(hud_scale(320, 240), 1);
        assert_eq!(hud_scale(1100, 680), 1);
        assert_eq!(hud_scale(1920, 1080), 2);
        assert_eq!(hud_scale(3840, 2160), 4);
        assert_eq!(hud_scale(100_000, 100_000), 4);
        assert_eq!(hud_scale(0, 0), 1);
    }

    #[test]
    fn a_fixture_says_so_in_exactly_the_agreed_words_and_real_content_never_does() {
        let mut i = input();
        i.fixture = true;
        i.area = "FIXTURE plaza".into();
        let list = draw(&i, 1100, 680);
        assert!(list.texts().iter().any(|t| t.text == "FIXTURE (synthetic, not FF7)"), "{:?}", list.texts());
        assert_eq!(FIXTURE_LABEL, "FIXTURE (synthetic, not FF7)");
        // It sits on a hazard-yellow plate, below the banner the renderer draws at the top.
        let t = list.texts().iter().find(|t| t.text == FIXTURE_LABEL).unwrap();
        assert!(t.y >= BANNER_PX as i32, "below the banner, at y {}", t.y);
        let plate = list.quads().iter().find(|q| q.color == FIXTURE_BG).expect("a yellow plate");
        assert!(plate.x <= t.x && plate.y <= t.y);
        // Real content: nothing says FIXTURE.
        let real = draw(&input(), 1100, 680);
        assert!(!real.says("FIXTURE") && real.says("FF7 md1stin"));
        // The label shows at every size, inside the window.
        for (w, h) in SIZES {
            let list = draw(&i, w, h);
            assert!(list.says(FIXTURE_LABEL), "{w}x{h}");
            assert_inside(&list, w, h, "fixture");
        }
    }

    #[test]
    fn the_fixture_label_and_the_area_name_never_overlap() {
        let mut i = input();
        i.fixture = true;
        i.area = "FIXTURE a_long_area_name".into();
        for (w, h) in SIZES {
            let list = draw(&i, w, h);
            let area = list.texts().iter().find(|t| t.text.starts_with("FIXTURE a_long")).unwrap();
            let label = list.texts().iter().find(|t| t.text == FIXTURE_LABEL && t.color == FIXTURE_INK).unwrap();
            assert!(!overlaps(rect_of(area), rect_of(label)), "{w}x{h}: {:?} vs {:?}", rect_of(area), rect_of(label));
        }
    }

    #[test]
    fn the_status_shows_the_connection_players_and_rtt() {
        let mut i = input();
        i.players = 3;
        let waiting = draw(&i, 1100, 680);
        assert!(waiting.says("ONLINE (waiting for the first reply)") && waiting.says("players 3") && waiting.says("rtt -"));
        i.rtt = Some(Duration::from_millis(12));
        let list = draw(&i, 1100, 680);
        assert!(list.texts().iter().any(|t| t.text == "ONLINE" && t.color == GOOD));
        assert!(list.says("players 3") && list.says("rtt 12 ms"));
        assert!(list.says("WASD move") && list.says("Esc x2 quit"), "the keys are listed");
        // On a narrow window the key list gives way instead of overlapping the status.
        let narrow = draw(&i, 320, 240);
        assert!(!narrow.says("WASD") && narrow.says("ONLINE"));
        assert_inside(&narrow, 320, 240, "status");
    }

    #[test]
    fn the_party_is_listed_with_its_leader_and_the_local_player_marked() {
        let mut i = input();
        i.party = vec![
            PartyRow { name: "Cloud".into(), leader: true, you: true },
            PartyRow { name: "Tifa".into(), leader: false, you: false },
            PartyRow { name: "A Name That Is Far Too Long To Fit".into(), leader: false, you: false },
        ];
        let list = draw(&i, 1100, 680);
        assert!(list.says("PARTY (3)"));
        assert!(list.says("* Cloud (you)"), "{:?}", list.texts().iter().map(|t| &t.text).collect::<Vec<_>>());
        assert!(list.texts().iter().any(|t| t.text == "  Tifa"));
        assert!(list.texts().iter().any(|t| t.text.starts_with("  A Name That Is F") && t.text.ends_with("...")), "long names are cut");
        // The panel is in the top right corner.
        let header = list.texts().iter().find(|t| t.text.starts_with("PARTY")).unwrap();
        assert!(header.x > 1100 / 2, "right half: x {}", header.x);
        assert!(header.y < 100);
        // Nobody in a party: no panel.
        assert!(!draw(&input(), 1100, 680).says("PARTY"));
        for (w, h) in SIZES {
            assert_inside(&draw(&i, w, h), w, h, "party");
        }
    }

    #[test]
    fn a_pending_invite_has_its_own_line() {
        let mut i = input();
        i.invite_from = Some("Barret".into());
        let list = draw(&i, 1100, 680);
        assert!(list.says("INVITE from Barret: press Y to join"));
        // (Each line is drawn twice: the shadow first, then the text.)
        let toast = list.texts().iter().find(|t| t.text.starts_with("INVITE") && t.color == ACCENT);
        assert!(toast.is_some(), "{:?}", list.texts());
        assert!(!draw(&input(), 1100, 680).says("INVITE"));
        for (w, h) in SIZES {
            assert_inside(&draw(&i, w, h), w, h, "invite");
        }
    }

    // ---- the local player's character ------------------------------------------------------------

    fn line(text: &str, level_up: bool) -> ProgressLine {
        ProgressLine { text: text.into(), level_up }
    }

    fn draw_with(input: &HudInput, progress: Option<&ProgressLine>, w: u32, h: u32) -> DrawList {
        let mut list = DrawList::new();
        draw_hud_with(input, progress, w, h, &mut list);
        list
    }

    #[test]
    fn the_character_line_sits_under_the_area_name_and_without_one_nothing_changes() {
        let i = input();
        let plain = draw(&i, 1100, 680);
        assert_eq!(draw_with(&i, None, 1100, 680).texts(), plain.texts(), "no line, the same HUD as before");
        assert!(!plain.says(" Lv "));

        let list = draw_with(&i, Some(&line("Wedge (Gunner)  Lv 7  EXP 626/949  Gil 20", false)), 1100, 680);
        assert!(list.says("Wedge (Gunner)  Lv 7  EXP 626/949  Gil 20") && list.says("FF7 md1stin"));
        let area = list.texts().iter().find(|t| t.text == "FF7 md1stin").unwrap();
        let mine = list.texts().iter().find(|t| t.text.starts_with("Wedge (Gunner)  Lv 7")).unwrap();
        assert!(mine.y > area.y, "below the area name ({} vs {})", mine.y, area.y);
        assert_eq!(mine.x, area.x, "on the same plate, at the same indent");
        assert!(!list.says("LEVEL UP"), "only when a level was just gained");
        for (w, h) in SIZES {
            assert_inside(&draw_with(&i, Some(&line("Wedge (Gunner)  Lv 7  EXP 626/949  Gil 20", true)), w, h), w, h, "character line");
        }
    }

    #[test]
    fn a_level_just_gained_is_announced_on_its_own_line() {
        let list = draw_with(&input(), Some(&line("Wedge (Gunner)  Lv 7  EXP 626/949  Gil 20", true)), 1100, 680);
        let mine = list.texts().iter().find(|t| t.text.starts_with("Wedge (Gunner)  Lv 7")).unwrap();
        let up = list.texts().iter().find(|t| t.text == "LEVEL UP!" && t.color == PARTY_RING).expect("a gold line");
        assert!(up.y > mine.y);
    }

    #[test]
    fn a_long_character_line_is_cut_to_the_window_and_the_fixture_label_clears_the_plate() {
        let long = "Wedge (Gunner)  Lv 99  EXP 99999999/99999999  Gil 9999999  and then some more words that cannot fit";
        for (w, h) in SIZES {
            assert_inside(&draw_with(&input(), Some(&line(long, true)), w, h), w, h, "long character line");
        }
        // A fixture's label drops below the plate when they would collide, which is taller now.
        let mut i = input();
        i.fixture = true;
        i.area = "FIXTURE a_long_area_name".into();
        for (w, h) in SIZES {
            let list = draw_with(&i, Some(&line("Wedge (Gunner)  Lv 7  EXP 6/33  Gil 0", true)), w, h);
            let label = list.texts().iter().find(|t| t.text == FIXTURE_LABEL && t.color == FIXTURE_INK).unwrap();
            for t in list.texts().iter().filter(|t| t.text.starts_with("Wedge (Gunner)  Lv") || t.text == "LEVEL UP!" || t.text.starts_with("FIXTURE a_long")) {
                assert!(!overlaps(rect_of(t), rect_of(label)), "{w}x{h}: {:?} vs the fixture label", t.text);
            }
        }
    }

    #[test]
    fn the_session_gives_the_line_once_the_server_has_spoken() {
        let mut s = state_with(&[]);
        s.progress.set_player("Wedge");
        assert_eq!(progress_line(&s, 5.0), None, "nothing said: no line");
        s.apply(5.0, ServerMsg::CharacterProgress { character: 1, level: 6, exp: 82, exp_to_next: 2, gil: 25, ap: 0, leveled_up: false });
        assert_eq!(progress_line(&s, 5.1), Some(ProgressLine { text: "Wedge (Gunner)  Lv 6  EXP 82/84  Gil 25".into(), level_up: false }));
        s.apply(9.0, ServerMsg::CharacterProgress { character: 1, level: 7, exp: 626, exp_to_next: 323, gil: 20, ap: 4, leveled_up: true });
        assert_eq!(progress_line(&s, 9.5), Some(ProgressLine { text: "Wedge (Gunner)  Lv 7  EXP 626/949  Gil 20".into(), level_up: true }));
        assert!(!progress_line(&s, 20.0).unwrap().level_up, "the announcement ends");
    }

    // ---- name labels -----------------------------------------------------------------------------

    #[test]
    fn a_label_is_centred_above_its_marker_in_the_right_colour() {
        let mut i = input();
        i.labels = vec![
            label(0.0, 0.0, "Cloud", LabelKind::Own),
            label(200.0, 100.0, "Tifa", LabelKind::Party),
            label(-200.0, -100.0, "Barret", LabelKind::Other),
        ];
        let (w, h) = (1100_u32, 680_u32);
        let list = draw(&i, w, h);
        for (name, x, y, tone) in [
            ("Cloud", 0.0, 0.0, [255, 255, 255, 255]),
            ("Tifa", 200.0, 100.0, PARTY_RING),
            ("Barret", -200.0, -100.0, OTHER),
        ] {
            let t = list.texts().iter().find(|t| t.text == name && t.color == tone).unwrap_or_else(|| panic!("{name}: {:?}", list.texts()));
            let [px, py] = view_to_window(&bounds(), w, h, x, y);
            let mid = (rect_of(t).0 + rect_of(t).2) as f32 / 2.0;
            assert!((mid - px).abs() <= 1.0, "{name} is centred on its marker: {mid} vs {px}");
            let bottom = rect_of(t).3 as f32;
            assert!(bottom < py - 12.0 && bottom > py - 40.0, "{name} sits just above its marker: label bottom {bottom}, marker {py}");
        }
        // A plate sits behind each label, so it is readable over any picture.
        assert!(list.quads().iter().any(|q| q.color == [0, 0, 0, 150]));
    }

    #[test]
    fn the_own_label_is_drawn_last_so_nothing_hides_it() {
        let mut i = input();
        i.labels = vec![label(0.0, 0.0, "Me", LabelKind::Own), label(0.0, 0.0, "Other", LabelKind::Other)];
        let list = draw(&i, 1100, 680);
        let names: Vec<&str> = list.texts().iter().map(|t| t.text.as_str()).filter(|t| *t == "Me" || *t == "Other").collect();
        // Each is drawn as shadow + text; the own label's pair comes after the other's.
        assert_eq!(names, ["Other", "Other", "Me", "Me"]);
    }

    #[test]
    fn labels_are_kept_inside_the_window_even_at_its_edges() {
        let mut i = input();
        i.labels = vec![
            label(-400.0, -300.0, "Bottom Left Corner Name", LabelKind::Other),
            label(400.0, 300.0, "Top Right Corner Name", LabelKind::Other),
            label(-10_000.0, 10_000.0, "Far Away", LabelKind::Other),
        ];
        for (w, h) in SIZES {
            assert_inside(&draw(&i, w, h), w, h, "edge labels");
        }
    }

    #[test]
    fn somebody_the_field_camera_has_out_of_the_window_gets_no_label_but_a_head_over_an_edge_keeps_its_own() {
        let mut i = input();
        i.labels = vec![
            label(0.0, 0.0, "Here", LabelKind::Party),
            label(-10_000.0, 0.0, "Far Left", LabelKind::Other),
            label(0.0, 10_000.0, "Far Above", LabelKind::Party),
            label(0.0, 340.0, "Just Over", LabelKind::Other), // about 18 pixels over the top edge
        ];
        let list = draw(&i, 1100, 680);
        let shown = |name: &str| list.texts().iter().any(|t| t.text == name);
        assert!(shown("Here") && shown("Just Over"));
        assert!(!shown("Far Left") && !shown("Far Above"));
        assert_inside(&list, 1100, 680, "kept labels");
    }

    #[test]
    fn labels_that_cannot_be_placed_are_skipped_not_forwarded() {
        let mut i = input();
        i.labels = vec![
            label(f32::NAN, 0.0, "NaN", LabelKind::Other),
            label(0.0, f32::INFINITY, "Inf", LabelKind::Other),
            label(0.0, 0.0, "", LabelKind::Other),
            label(0.0, 0.0, "Fine", LabelKind::Other),
        ];
        let list = draw(&i, 1100, 680);
        assert!(!list.says("NaN") && !list.says("Inf"));
        assert!(list.says("Fine"));
        assert!(list.vertices(1100, 680, false).iter().all(|v| v.pos.iter().chain(v.uv.iter()).all(|c| c.is_finite())));
        // Degenerate bounds do not poison the labels either.
        let mut j = input();
        j.bounds = Bounds { min: Pos::new(f32::NAN, 0.0, 0.0), max: Pos::new(1.0, f32::INFINITY, 0.0) };
        j.labels = vec![label(0.0, 0.0, "Odd", LabelKind::Other)];
        assert!(draw(&j, 640, 360).vertices(640, 360, false).iter().all(|v| v.pos.iter().all(|c| c.is_finite())));
    }

    #[test]
    fn long_names_are_cut_to_a_label() {
        let mut i = input();
        i.labels = vec![label(0.0, 0.0, &"W".repeat(24), LabelKind::Other)];
        let list = draw(&i, 1100, 680);
        let t = list.texts().iter().find(|t| t.text.starts_with("WWW")).unwrap();
        assert!(cell_count(&t.text) <= NAME_CELLS && t.text.ends_with("..."), "{:?}", t.text);
    }

    #[test]
    fn the_hud_scales_up_with_big_windows() {
        let mut i = input();
        i.labels = vec![label(0.0, 0.0, "Cloud", LabelKind::Own)];
        let small = draw(&i, 1100, 680);
        let big = draw(&i, 1920, 1080);
        assert_eq!(small.texts()[0].scale, 1);
        assert!(big.texts().iter().all(|t| t.scale == 2));
        assert_inside(&big, 1920, 1080, "big");
    }

    #[test]
    fn random_huds_never_leave_the_window_or_forward_garbage() {
        use crate::ui::testkit::Rng;
        for seed in 1..=40 {
            let mut rng = Rng::new(seed);
            let coord = |rng: &mut Rng| match rng.below(6) {
                0 => f32::NAN,
                1 => f32::INFINITY,
                2 => -10_000.0,
                3 => 10_000.0,
                _ => (rng.below(1000) as f32) - 500.0,
            };
            let labels = (0..rng.below(30))
                .map(|_| Label {
                    x: coord(&mut rng),
                    y: coord(&mut rng),
                    text: rng.string(30),
                    kind: [LabelKind::Own, LabelKind::Party, LabelKind::Other][rng.below(3)],
                })
                .collect();
            let input = HudInput {
                bounds: if rng.chance(8) {
                    Bounds { min: Pos::new(f32::NAN, 0.0, 0.0), max: Pos::new(1.0, 1.0, 0.0) }
                } else {
                    bounds()
                },
                labels,
                area: rng.string(60),
                fixture: rng.chance(2),
                players: rng.below(500),
                rtt: rng.chance(2).then(|| Duration::from_millis(rng.below(100_000) as u64)),
                party: (0..rng.below(17))
                    .map(|_| PartyRow { name: rng.string(30), leader: rng.chance(4), you: rng.chance(4) })
                    .collect(),
                invite_from: rng.chance(3).then(|| rng.string(40)),
            };
            for (w, h) in SIZES {
                let list = draw(&input, w, h);
                assert_inside(&list, w, h, &format!("seed {seed}"));
                assert!(
                    list.vertices(w, h, false).iter().all(|v| v.pos.iter().chain(v.uv.iter()).all(|c| c.is_finite())),
                    "seed {seed}: a non-finite vertex"
                );
            }
        }
    }

    // ---- gathering from the live state -----------------------------------------------------------

    fn state_with(peers: &[(u64, f32, &str)]) -> ClientState {
        let area = AreaId { game: GameId::Fixture, key: "plaza".into() };
        let mut s = ClientState::new(EntityId(1), AccountId(1), Pos::new(0.0, 0.0, 0.0), area);
        let entities = peers
            .iter()
            .map(|(id, x, name)| EntitySnapshot {
                id: EntityId(*id),
                pos: Pos::new(*x, 0.0, 0.0),
                facing: 0.0,
                anim: AnimState::Idle,
                avatar: AvatarAppearance::new(*name, [1, 2, 3], None),
            })
            .collect();
        s.apply(0.0, ServerMsg::EntityUpdate { tick: 1, entities });
        s
    }

    fn info(fixture: bool) -> SessionInfo {
        SessionInfo {
            name: "Cloud".into(),
            tint: [9, 9, 9],
            area_id: AreaId { game: if fixture { GameId::Fixture } else { GameId::Ff7 }, key: "plaza".into() },
            fixture,
            card: fmv_protocol::CharacterCard::plain("Cloud", [9, 9, 9]),
        }
    }

    fn top_down() -> FieldView {
        let adapter = FixtureAdapter::new();
        let area = adapter.load_area("plaza").expect("the fixture's plaza");
        FieldView::top_down(area.as_ref())
    }

    fn party_event(s: &mut ClientState, seq: u64, event: PartyEvent) {
        let names = vec![
            PartyName { account: AccountId(1), name: "Cloud".into(), entity: Some(EntityId(1)) },
            PartyName { account: AccountId(2), name: "Tifa".into(), entity: Some(EntityId(2)) },
            PartyName { account: AccountId(3), name: "Barret".into(), entity: Some(EntityId(3)) },
        ];
        s.apply(0.0, ServerMsg::PartyEvent { seq, event, names });
    }

    #[test]
    fn gathering_labels_every_visible_player_by_the_name_the_server_gave() {
        let s = state_with(&[(2, 10.0, "Tifa"), (3, -20.0, "Barret")]);
        let hud = gather_from(&s, &info(true), &top_down(), 1.0);
        let mut names: Vec<(&str, LabelKind)> = hud.labels.iter().map(|l| (l.text.as_str(), l.kind)).collect();
        names.sort_by_key(|(name, _)| *name);
        assert_eq!(
            names,
            [("Barret", LabelKind::Other), ("Cloud", LabelKind::Own), ("Tifa", LabelKind::Other)]
        );
        assert_eq!(hud.players, 3);
        assert!(hud.fixture && hud.area == "FIXTURE plaza");
        assert!(hud.party.is_empty() && hud.invite_from.is_none() && hud.rtt.is_none());
        // Every label has a finite place in view space.
        assert!(hud.labels.iter().all(|l| l.x.is_finite() && l.y.is_finite()));
    }

    /// The area's characters are named where the anchor says (over the head of a model, at the
    /// marker otherwise), and one the anchor cannot place has no label.
    #[test]
    fn an_npc_is_named_where_the_anchor_puts_its_name_and_one_that_cannot_be_placed_is_not_named() {
        let mut s = state_with(&[]);
        let npc = |id: u32, name: &str, x: f32| fmv_protocol::NpcSnapshot {
            id,
            name: name.into(),
            model: "agyb".into(),
            idle_animation: Some("acfe".into()),
            pos: Pos::new(x, 5.0, 0.0),
            facing: 0.0,
        };
        let area = AreaId { game: GameId::Fixture, key: "plaza".into() };
        let tifa = EntitySnapshot {
            id: EntityId(2),
            pos: Pos::new(10.0, 0.0, 0.0),
            facing: 0.0,
            anim: AnimState::Idle,
            avatar: AvatarAppearance::new("Tifa", [1, 2, 3], None),
        };
        s.apply(0.0, ServerMsg::AreaSnapshot { area, tick: 1, entities: vec![tifa], npcs: vec![npc(0, "gu0", 30.0), npc(1, "gu1", -30.0)] });
        let (info, view) = (info(true), top_down());
        let name_of = |hud: &HudInput, text: &str| hud.labels.iter().find(|l| l.text == text).map(|l| (l.x, l.y, l.kind));
        // The marker view: at the marker, whoever it is.
        let plain = gather_from(&s, &info, &view, 1.0);
        let at_marker = view.to_view(Pos::new(30.0, 5.0, 0.0)).unwrap();
        assert_eq!(name_of(&plain, "gu0"), Some((at_marker.x, at_marker.y, LabelKind::Npc)));
        assert!(name_of(&plain, "gu1").is_some());
        // The model view: the anchor is asked about the NPC by its index and says where its head is.
        let over_head = Pos::new(1.0, 2.0, 0.0);
        let asked = std::cell::RefCell::new(Vec::new());
        let anchored = gather_anchored(&s, &info, &view, 1.0, &|subject, at| {
            asked.borrow_mut().push(subject);
            match subject {
                Subject::Npc(0) => Some(over_head),
                Subject::Npc(_) => None, // cannot be drawn: no label
                _ => view.to_view(at),
            }
        });
        assert_eq!(name_of(&anchored, "gu0"), Some((1.0, 2.0, LabelKind::Npc)));
        assert_eq!(name_of(&anchored, "gu1"), None);
        assert!(asked.borrow().contains(&Subject::Npc(0)) && asked.borrow().contains(&Subject::Npc(1)));
        assert!(asked.borrow().contains(&Subject::Remote(EntityId(2))) && asked.borrow().contains(&Subject::Own));
        assert!(name_of(&anchored, "Tifa").is_some() && name_of(&anchored, "Cloud").is_some(), "players are named as before");
    }

    #[test]
    fn gathering_knows_the_party_its_leader_and_a_pending_invite() {
        let mut s = state_with(&[(2, 10.0, "Tifa"), (3, 20.0, "Barret")]);
        s.set_rtt(Duration::from_millis(7));
        party_event(&mut s, 1, PartyEvent::PartyFormed { party: PartyId(1), invite: InviteId(1), leader: AccountId(2), members: vec![AccountId(1), AccountId(2)] });
        party_event(&mut s, 2, PartyEvent::InviteSent { invite: InviteId(2), from: AccountId(3), to: AccountId(1), expires_at: 9 });
        let hud = gather_from(&s, &info(false), &top_down(), 1.0);
        assert_eq!(
            hud.party,
            [
                PartyRow { name: "Cloud".into(), leader: false, you: true },
                PartyRow { name: "Tifa".into(), leader: true, you: false },
            ]
        );
        assert_eq!(hud.invite_from.as_deref(), Some("Barret"));
        assert_eq!(hud.rtt, Some(Duration::from_millis(7)));
        let kind_of = |name: &str| hud.labels.iter().find(|l| l.text == name).unwrap().kind;
        assert_eq!(kind_of("Tifa"), LabelKind::Party, "party members get the gold label");
        assert_eq!(kind_of("Barret"), LabelKind::Other);
        assert_eq!(kind_of("Cloud"), LabelKind::Own);
        assert!(!hud.fixture && hud.area == "FF7 plaza");
        // And all of it draws.
        let list = draw(&hud, 1100, 680);
        assert!(list.says("PARTY (2)") && list.says("* Tifa") && list.says("  Cloud (you)") && list.says("INVITE from Barret"));
        assert_inside(&list, 1100, 680, "gathered");
    }
}
