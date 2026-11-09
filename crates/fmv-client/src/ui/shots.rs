//! Offscreen screenshots of the character menu and the HUD, drawn through the real renderer
//! (`Gpu`, the UI pipeline, the font atlas) into a texture and written as PNGs to
//! `~/.cache/claude-tmp/fmv-client-shots/` (or `$FMV_SHOT_DIR`), where a person can look at them:
//! this machine has no display, so this is how window mode is checked.
//!
//! They need a GPU or a software Vulkan/GL driver (lavapipe/llvmpipe), so they only run on request:
//! `cargo test -p fmv-client -- --ignored offscreen`. Besides writing the pictures they check
//! pixels: the palette, the swatches and the plates really reach the screen.

use super::draw::DrawList;
use super::hud::{self, HudInput, Label, LabelKind, PartyRow};
use super::menu::{CharacterEntry, KeyPress, Look, Menu, MenuConfig, NavKey, Notice, Screen};
use super::palette;
use super::screens;
use super::testkit::{empty_scene, save, Offscreen};
use crate::backdrop::FieldView;
use crate::render::{PlayerSprite, Scene};
use fmv_adapter_api::GameAdapter;
use fmv_core::{GameId, Pos};
use fmv_fixture::FixtureAdapter;
use std::time::Duration;

const W: u32 = 1100;
const H: u32 = 680;

fn entry(name: &str, tint: Option<[u8; 3]>, look: Option<&str>) -> CharacterEntry {
    CharacterEntry {
        name: name.into(),
        look: tint.map(|t| Look::plain(t, look)),
        loose_permissions: false,
    }
}

fn accounts() -> Vec<CharacterEntry> {
    vec![
        entry("Cloud", Some([90, 125, 245]), Some("ff7/cloud")),
        entry("Tifa", Some([235, 80, 80]), Some("ff7/tifa")),
        entry("Aerith", Some([250, 150, 185]), Some("ff7/aerith")),
        entry("Red XIII", None, None),
    ]
}

fn menu(game: GameId, accounts: Vec<CharacterEntry>) -> Menu {
    Menu::new(MenuConfig::new("127.0.0.1:47770", game), accounts)
}

fn press(m: &mut Menu, key: NavKey) {
    m.key(KeyPress::new(key));
}

/// Draw `menu` at `w` x `h`, save it as `<name>.png` and return the pixels.
fn shoot(gpu: &mut Offscreen, name: &str, menu: &Menu, w: u32, h: u32) -> Vec<u8> {
    let mut list = DrawList::new();
    screens::draw_menu(menu, w, h, &mut list);
    let img = gpu.render(w, h, &empty_scene(), &list);
    save(name, w, h, &img);
    img
}

/// The tint of the form on screen.
fn form_tint(m: &Menu) -> [u8; 3] {
    match m.screen() {
        Screen::Create(f) => f.tint,
        other => panic!("not the create form: {other:?}"),
    }
}

fn count(img: &[u8], rgb: [u8; 3]) -> usize {
    img.chunks(4).filter(|p| p[..3] == rgb).count()
}

/// How many pixels are light enough to be text.
fn bright(img: &[u8]) -> usize {
    img.chunks(4).filter(|p| p[0] > 200 && p[1] > 200 && p[2] > 200).count()
}

#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_character_select_screens() {
    let Some(mut gpu) = Offscreen::new() else { return };

    // The list with a few characters; the second one highlighted.
    let mut m = menu(GameId::Ff7, accounts());
    press(&mut m, NavKey::Down);
    let img = shoot(&mut gpu, "01-select", &m, W, H);
    assert!(bright(&img) > 1500, "there is text on the screen");
    // The swatches of the characters with a known look are their exact colours.
    for tint in [[90, 125, 245], [235, 80, 80], [250, 150, 185]] {
        assert!(count(&img, tint) > 100, "swatch {tint:?} is on screen");
    }

    // Forgetting asks first.
    press(&mut m, NavKey::Delete);
    let img = shoot(&mut gpu, "02-select-forget-confirm", &m, W, H);
    assert!(bright(&img) > 800);

    // A refused login comes back to the list with the reason.
    let mut m = menu(GameId::Ff7, accounts());
    press(&mut m, NavKey::Enter);
    m.tick(2.0);
    shoot(&mut gpu, "03-connecting", &m, W, H);
    m.connect_failed(
        Notice::error("The server refused: invalid or unknown token")
            .with_hint("The server does not know this saved login (its accounts may have been reset). Press Del to forget it, then N to create a new character."),
    );
    shoot(&mut gpu, "04-select-login-refused", &m, W, H);

    // The fixture says what it is.
    let m = menu(GameId::Fixture, accounts());
    shoot(&mut gpu, "05-select-fixture", &m, W, H);

    // A long list scrolls.
    let many: Vec<CharacterEntry> = (0..14).map(|i| entry(&format!("Hero {i:02}"), Some(palette::PALETTE[i % 16]), None)).collect();
    let mut m = menu(GameId::Ff7, many);
    for _ in 0..9 {
        press(&mut m, NavKey::Down);
    }
    shoot(&mut gpu, "06-select-scrolled", &m, W, H);

    // Other window sizes: the smallest, the design size, a big one.
    let m = menu(GameId::Ff7, accounts());
    shoot(&mut gpu, "07-select-320x240", &m, 320, 240);
    shoot(&mut gpu, "08-select-640x360", &m, 640, 360);
    shoot(&mut gpu, "09-select-1920x1080", &m, 1920, 1080);
}

#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_character_create_screens() {
    let Some(mut gpu) = Offscreen::new() else { return };

    // A fresh install opens on the form. Type a name, move to the body and pick the next one.
    let mut m = menu(GameId::Ff7, vec![]);
    m.text("Cloud");
    let img = shoot(&mut gpu, "10-create-name", &m, W, H);
    // The whole palette is on screen, every colour exactly.
    for (i, tint) in palette::PALETTE.iter().enumerate() {
        assert!(count(&img, *tint) > 20, "palette colour {i} {tint:?} is drawn");
    }
    press(&mut m, NavKey::Tab);
    press(&mut m, NavKey::Right);
    shoot(&mut gpu, "11-create-body", &m, W, H);

    // The palette focused with a colour chosen: the preview swatch follows.
    press(&mut m, NavKey::Tab);
    press(&mut m, NavKey::Right);
    press(&mut m, NavKey::Right);
    let img = shoot(&mut gpu, "12-create-palette", &m, W, H);
    assert!(count(&img, form_tint(&m)) > 20, "the chosen colour is on screen (palette cell and preview)");

    // A slider focused.
    for _ in 0..2 {
        press(&mut m, NavKey::Tab);
    }
    press(&mut m, NavKey::Right);
    press(&mut m, NavKey::Right);
    shoot(&mut gpu, "13-create-slider", &m, W, H);

    // The server refused the name: the message is on the form, ready for another try.
    let mut m = menu(GameId::Ff7, accounts());
    m.text("n");
    m.text("Cloud");
    press(&mut m, NavKey::Enter);
    m.tick(1.0);
    m.connect_failed(Notice::error("The server refused: that display name is already taken").with_hint("Change the name and press Enter again."));
    shoot(&mut gpu, "14-create-name-taken", &m, W, H);

    // Registration closed.
    let mut m = menu(GameId::Ff7, vec![]);
    m.text("Tifa");
    press(&mut m, NavKey::Enter);
    m.connect_failed(
        Notice::error("The server refused: registration is closed on this server; log in with a saved token")
            .with_hint("This server takes no new characters. Press Esc and play a saved one."),
    );
    shoot(&mut gpu, "15-create-registration-closed", &m, W, H);

    // A non-ASCII name: boxes for what the font lacks, with a note.
    let mut m = menu(GameId::Ff7, vec![]);
    m.text("Jos\u{e9} \u{4e2d}");
    shoot(&mut gpu, "16-create-unsupported-characters", &m, W, H);

    // The fixture has no bodies and no slots.
    let mut m = menu(GameId::Fixture, vec![]);
    m.text("Bot");
    shoot(&mut gpu, "17-create-fixture", &m, W, H);

    // The token that could not be saved takes the screen.
    let mut m = menu(GameId::Ff7, vec![]);
    m.set_notice(
        Notice::error("The character was created on the server, but its login could not be saved on this computer (disk full).")
            .with_hint("Write the token below into a file by hand: it is shown only once.")
            .with_secret("fmvt_7f3a9c1e5b2d4a60918e3c7d5f21b4a8"),
    );
    shoot(&mut gpu, "18-token-not-saved", &m, W, H);

    // Loading.
    let mut m = menu(GameId::Ff7, accounts());
    m.loading("FF7 md1stin");
    shoot(&mut gpu, "19-loading", &m, W, H);

    // Small and big windows.
    let m = menu(GameId::Ff7, vec![]);
    shoot(&mut gpu, "20-create-320x240", &m, 320, 240);
    shoot(&mut gpu, "21-create-640x360", &m, 640, 360);
    shoot(&mut gpu, "22-create-1920x1080", &m, 1920, 1080);
}

/// The create screen as a player sees it: the Body and Slot rows, each with the line that says what it means under it
/// (`creator.png` in `$FMV_SHOT_DIR`, at the window's default size). The other sizes (`creator-<w>x<h>.png`) show how
/// the lines give way where the window has no room for both: the help of the focused row takes the place of the key
/// hints under the buttons.
#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_creator() {
    use super::widgets::DIM;
    let Some(mut gpu) = Offscreen::new() else { return };
    let mut m = menu(GameId::Ff7, vec![]);
    m.text("Wedge");
    press(&mut m, NavKey::Tab); // the body
    press(&mut m, NavKey::Right); // the next one
    press(&mut m, NavKey::Tab); // the colour
    for _ in 0..3 {
        press(&mut m, NavKey::Right);
    }
    for _ in 0..4 {
        press(&mut m, NavKey::Tab); // red, green, blue, the slot
    }
    press(&mut m, NavKey::Right); // Tifa's slot
    let img = shoot(&mut gpu, "creator", &m, W, H);
    let mut list = DrawList::new();
    screens::draw_menu(&m, W, H, &mut list);
    for needle in ["Name", "Body", "Slot", "Tifa Lockhart", "how you look on the field (a placeholder model)", "who you fight as: its combat moves and level curve", "(fixed once created)"] {
        assert!(list.says(needle), "{needle:?} is on the screen");
    }
    // The help lines are DIM text under their rows, and they reach the screen.
    let dim = img.chunks(4).filter(|p| p[..3] == [DIM[0], DIM[1], DIM[2]]).count();
    assert!(dim > 300, "the help lines are drawn: {dim} pixels of the dim text colour");
    for (w, h) in [(640, 360), (1920, 1080), (320, 240)] {
        let img = shoot(&mut gpu, &format!("creator-{w}x{h}"), &m, w, h);
        assert!(bright(&img) > 500, "{w}x{h}: there is text on the screen");
    }
}

/// The HUD over a fixture area seen from above: names, the party, the status and the FIXTURE label.
#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_hud_over_the_fixture_area() {
    let Some(mut gpu) = Offscreen::new() else { return };
    let adapter = FixtureAdapter::new();
    let area = adapter.load_area("plaza").expect("the fixture's plaza");
    let view = FieldView::top_down(area.as_ref());
    let spawn = area.spawn_points()[0];
    let at = |dx: f32, dy: f32| view.to_view(Pos::new(spawn.x + dx, spawn.y + dy, spawn.z)).expect("on screen");
    let spots = [(0.0, 0.0, "Cloud", LabelKind::Own, [90, 125, 245]), (120.0, 40.0, "Tifa", LabelKind::Party, [235, 80, 80]), (-150.0, -60.0, "Barret", LabelKind::Other, [60, 200, 90]), (40.0, -120.0, "Red XIII", LabelKind::Other, [255, 190, 40])];
    let sprites: Vec<PlayerSprite> = spots
        .iter()
        .map(|(dx, dy, _, kind, tint)| {
            let p = at(*dx, *dy);
            PlayerSprite { x: p.x, y: p.y, tint: *tint, own: *kind == LabelKind::Own, party: *kind != LabelKind::Other }
        })
        .collect();
    let labels: Vec<Label> = spots
        .iter()
        .map(|(dx, dy, name, kind, _)| {
            let p = at(*dx, *dy);
            Label { x: p.x, y: p.y, text: (*name).into(), kind: *kind }
        })
        .collect();
    let input = HudInput {
        bounds: view.bounds,
        labels,
        area: "FIXTURE plaza".into(),
        fixture: true,
        players: 4,
        rtt: Some(Duration::from_millis(12)),
        party: vec![PartyRow { name: "Cloud".into(), leader: true, you: true }, PartyRow { name: "Tifa".into(), leader: false, you: false }],
        invite_from: Some("Barret".into()),
    };
    let scene = Scene { bounds: view.bounds, triangles: &view.triangles, players: &sprites, fixture_banner: true, backdrop: None };
    let mut overlay = DrawList::new();
    hud::draw_hud(&input, W, H, &mut overlay);
    let img = gpu.render(W, H, &scene, &overlay);
    save("30-hud-fixture", W, H, &img);
    assert!(bright(&img) > 300, "text is on screen");
    // The hazard-yellow FIXTURE plate is drawn (the banner above it is yellow too, so look below it).
    let below_banner = &img[(24 * W * 4) as usize..(60 * W * 4) as usize];
    assert!(count(below_banner, [255, 196, 0]) > 500, "the FIXTURE label's plate is on screen");

    // The same HUD for a real FF7 field: no FIXTURE label, no banner.
    let mut real = input.clone();
    real.fixture = false;
    real.area = "FF7 md1stin".into();
    let scene = Scene { fixture_banner: false, ..scene };
    let mut overlay = DrawList::new();
    hud::draw_hud(&real, W, H, &mut overlay);
    let img = gpu.render(W, H, &scene, &overlay);
    save("31-hud-ff7", W, H, &img);
    assert_eq!(count(&img, [255, 196, 0]), 0, "nothing yellow-black about real content");

    // Character progress (protocol v5): the line under the area name, and the gold LEVEL UP! line for a
    // few seconds after a level was gained.
    let plain_bright = bright(&img);
    let plain_gold = count(&img, [255, 208, 64]);
    let mut overlay = DrawList::new();
    hud::draw_hud_with(&real, Some(&hud::ProgressLine { text: "Wedge (Barret)  Lv 7  EXP 626/949  Gil 20".into(), level_up: false }), W, H, &mut overlay);
    let with_line = gpu.render(W, H, &scene, &overlay);
    save("33-hud-ff7-character", W, H, &with_line);
    assert!(bright(&with_line) > plain_bright, "the character line puts more text on screen");
    let mut overlay = DrawList::new();
    hud::draw_hud_with(&real, Some(&hud::ProgressLine { text: "Wedge (Barret)  Lv 7  EXP 626/949  Gil 20".into(), level_up: true }), W, H, &mut overlay);
    let level_up = gpu.render(W, H, &scene, &overlay);
    save("34-hud-ff7-level-up", W, H, &level_up);
    assert!(count(&level_up, [255, 208, 64]) > plain_gold, "the gold LEVEL UP! text is on screen");

    // A bigger window gets a bigger HUD.
    let mut overlay = DrawList::new();
    hud::draw_hud(&input, 1920, 1080, &mut overlay);
    let scene = Scene { fixture_banner: true, ..scene };
    let img = gpu.render(1920, 1080, &scene, &overlay);
    save("32-hud-fixture-1920x1080", 1920, 1080, &img);
}

/// The HUD over a REAL FF7 field (the owner's own copy, opened read-only), on request: names,
/// party and status over the decoded background, as in the window. `FMV_FF7_LIBRARY` names the
/// state directory the library lives in (`~/.local/share/ff-multiverse`), `FMV_FF7_AREA` the
/// field (default `md1stin`). Nothing is written except the PNGs.
#[test]
#[ignore = "needs the owner's real FF7 library (FMV_FF7_LIBRARY) and a GPU or software Vulkan/GL driver"]
fn offscreen_hud_over_the_real_ff7_field() {
    use crate::ff7::{open_ff7, prewarm, Ff7Source};
    use fmv_adapter_api::AreaRuntime;
    let Some(library) = std::env::var_os("FMV_FF7_LIBRARY") else {
        eprintln!("skipped: FMV_FF7_LIBRARY is not set");
        return;
    };
    let Some(mut gpu) = Offscreen::new() else { return };
    let key = std::env::var("FMV_FF7_AREA").unwrap_or_else(|_| "md1stin".to_string());
    let opened = open_ff7(&Ff7Source::Library { state_dir: Some(library.into()), set: None }).expect("open the library");
    prewarm(&opened.adapter);
    let view = FieldView::ff7(&opened.adapter, &key).expect("the field has a camera and a walkmesh");
    let area = opened.adapter.load_field(&key).expect("field");
    let spawns = area.spawn_points();
    let names = [("Cloud", LabelKind::Own, [90, 125, 245]), ("Tifa", LabelKind::Party, [235, 80, 80]), ("Barret", LabelKind::Other, [60, 200, 90]), ("Aerith", LabelKind::Other, [250, 150, 185])];
    let mut sprites = Vec::new();
    let mut labels = Vec::new();
    for (i, (name, kind, tint)) in names.iter().enumerate() {
        // Spread the players over the walkmesh: the spawn points first, then points between them.
        let base = spawns[i % spawns.len()];
        let at = Pos::new(base.x + 40.0 * (i / spawns.len()) as f32, base.y + 25.0 * (i / spawns.len()) as f32, base.z);
        let Some(p) = view.to_view(at) else { continue };
        sprites.push(PlayerSprite { x: p.x, y: p.y, tint: *tint, own: *kind == LabelKind::Own, party: *kind != LabelKind::Other });
        labels.push(Label { x: p.x, y: p.y, text: (*name).into(), kind: *kind });
    }
    assert!(!sprites.is_empty(), "no player could be drawn");
    let input = HudInput {
        bounds: view.bounds,
        labels,
        area: hud::area_label(&fmv_core::AreaId { game: GameId::Ff7, key: key.clone() }, false),
        fixture: false,
        players: 4,
        rtt: Some(Duration::from_millis(14)),
        party: vec![PartyRow { name: "Cloud".into(), leader: true, you: true }, PartyRow { name: "Tifa".into(), leader: false, you: false }],
        invite_from: None,
    };
    let scene = Scene { bounds: view.bounds, triangles: &view.triangles, players: &sprites, fixture_banner: false, backdrop: view.backdrop.as_ref() };
    for (w, h) in [(1100_u32, 680_u32), (1920, 1080)] {
        let mut overlay = DrawList::new();
        hud::draw_hud(&input, w, h, &mut overlay);
        let img = gpu.render(w, h, &scene, &overlay);
        save(&format!("40-hud-real-ff7-{key}-{w}x{h}"), w, h, &img);
        assert!(bright(&img) > 300, "text is on screen");
    }
}
