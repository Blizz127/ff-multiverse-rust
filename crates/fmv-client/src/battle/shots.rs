//! Offscreen pictures of the battle UI, drawn through the real renderer (`Gpu`: the flat tokens, the
//! UI pipeline, the font atlas) into a texture and written as PNGs to `$FMV_SHOT_DIR` (default
//! `~/.cache/claude-tmp/fmv-client-shots/`), where a person can look at them: this machine has no
//! display, so this is how the battle screen is checked.
//!
//! They need a GPU or a software Vulkan/GL driver (lavapipe/llvmpipe), so they only run on request:
//! `FMV_SHOT_DIR=<dir> cargo test -p fmv-client -- --ignored --nocapture offscreen_battle`. A
//! machine without an adapter says "skipped" and passes; look for the `offscreen adapter:` line and
//! the `wrote` lines in the output. Besides writing the pictures they check pixels: the windows, the
//! text, the greyed-out rows, the pointer, the numbers and the panels really reach the screen.
//!
//! The views are made by the same code that dresses the live one ([`BattleHud`]); the names are made
//! up (no game data is read).

use super::hud::BattleHud;
use super::menu::{MenuItem, MenuKey, MenuLimit, MenuSpell, TargetRule};
use super::panels::{layout, Banner, Layout, RewardItem, RewardLine, DISABLED, PANEL_DELAY_MS};
use super::popups::{Popup, PopupKind};
use super::text::draw_battle_text;
use super::view::{battle_vertices, flat_anchors, Bar, BattleView, Status, LIMIT_FULL_RGB, LIMIT_RGB};
use crate::render::{Gpu, Vertex, CLEAR_RGB};
use crate::ui::draw::DrawList;
use crate::ui::testkit::save;
use crate::ui::widgets::{ACCENT, BORDER, ERROR, INFO, PANEL_BOTTOM, PANEL_TOP, TEXT, WARN};
use fmv_ff7_battle::rules::{ActorId, BattleEvent, Outcome};
use fmv_protocol::BattleId;

const SIZES: [(u32, u32); 2] = [(1100, 680), (1920, 1080)];
/// The seconds of the clock the shots live at.
const NOW: f64 = 100.0;

/// A GPU context without a window that draws the flat vertices and the overlay of a battle frame.
struct Shooter {
    device: wgpu::Device,
    queue: wgpu::Queue,
    gpu: Gpu,
}

impl Shooter {
    /// `None` (with a note on stderr) when no adapter is available.
    fn new() -> Option<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("skipped: no GPU adapter available ({e})");
                return None;
            }
        };
        eprintln!("offscreen adapter: {:?}", adapter.get_info().name);
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_limits: wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits()),
            ..wgpu::DeviceDescriptor::default()
        }))
        .expect("device");
        let gpu = Gpu::new(device.clone(), queue.clone(), wgpu::TextureFormat::Rgba8Unorm);
        Some(Self { device, queue, gpu })
    }

    /// Draw `vertices` (the flat layer) and `overlay` into a `w` x `h` target; tightly packed RGBA8.
    fn render(&mut self, w: u32, h: u32, vertices: &[Vertex], overlay: &DrawList) -> Vec<u8> {
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("battle-shot"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let target = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.gpu.draw_scene(&target, vertices, None, CLEAR_RGB, false);
        self.gpu.draw_overlay(&target, &overlay.vertices(w, h, false));
        let padded = (w * 4).next_multiple_of(256);
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("battle-shot-readback"),
            size: u64::from(padded * h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: &texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo { buffer: &readback, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: Some(h) } },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.queue.submit(std::iter::once(enc.finish()));
        let (tx, rx) = std::sync::mpsc::channel();
        readback.slice(..).map_async(wgpu::MapMode::Read, move |r| tx.send(r).expect("send"));
        self.device.poll(wgpu::PollType::wait_indefinitely()).expect("poll");
        rx.recv().expect("map callback").expect("map");
        let data = readback.slice(..).get_mapped_range().expect("mapped range");
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for row in data.chunks(padded as usize).take(h as usize) {
            out.extend_from_slice(&row[..(w * 4) as usize]);
        }
        out
    }

    /// Draw `view` the way the window does without a 3D scene (flat tokens and the overlay), save it
    /// as `<name>-<w>x<h>.png` and return the pixels.
    fn shoot(&mut self, name: &str, view: &BattleView, w: u32, h: u32) -> Vec<u8> {
        let mut overlay = DrawList::new();
        draw_battle_text(view, w, h, &mut overlay);
        let img = self.render(w, h, &battle_vertices(view, w, h, false), &overlay);
        save(&format!("{name}-{w}x{h}"), w, h, &img);
        img
    }
}

#[allow(clippy::too_many_arguments)] // a test fixture: one line per fighter reads best
fn bar(actor: u8, name: &str, hp: u32, max_hp: u32, mp: u32, max_mp: u32, gauge: u32, ready: bool, mine: bool) -> Bar {
    Bar { actor: ActorId(actor), name: name.into(), hp, max_hp, mp, max_mp, gauge, ready: ready && hp > 0, alive: hp > 0, mine, limit: 0 }
}

/// Three heroes (Barret is ours and ready, Tifa is hurt) against three enemies.
fn battle() -> BattleView {
    BattleView {
        id: BattleId(1),
        party: vec![
            bar(0, "Cloud", 427, 427, 59, 59, 41_000, false, false),
            bar(1, "Barret", 312, 480, 22, 40, 65_535, true, true),
            bar(2, "Tifa", 98, 390, 20, 50, 12_000, false, false),
        ],
        enemies: vec![
            bar(3, "Guard Hound", 55, 55, 0, 0, 0, false, false),
            bar(4, "Mono Drive", 140, 140, 0, 0, 0, false, false),
            bar(5, "Sweeper", 220, 220, 0, 0, 0, false, false),
        ],
        status: Status::Verified,
        ..BattleView::default()
    }
}

/// A hud for Barret with made-up spells and items: more spells than rows, one that costs more MP than
/// Barret has, an item that is gone.
fn hud() -> BattleHud {
    let mut hud = BattleHud::new(None);
    hud.begin(0, Some(ActorId(1)));
    let model = hud.model_mut();
    model.spells = vec![
        MenuSpell::new(1, "Fire", 4, TargetRule::Enemies),
        MenuSpell::new(2, "Ice", 4, TargetRule::Enemies),
        MenuSpell::new(3, "Bolt", 4, TargetRule::Enemies),
        MenuSpell::new(4, "Cure", 5, TargetRule::Allies),
        MenuSpell::new(5, "Fire 2", 22, TargetRule::Enemies),
        MenuSpell::new(6, "Haste", 18, TargetRule::Allies),
        MenuSpell::new(7, "Meteor", 40, TargetRule::Enemies),
    ];
    model.items = vec![
        MenuItem::new(1, "Potion", 7, TargetRule::Allies),
        MenuItem::new(2, "Hi-Potion", 2, TargetRule::Allies),
        MenuItem::new(3, "Ether", 1, TargetRule::Allies),
        MenuItem::new(4, "Phoenix Down", 3, TargetRule::DeadAllies),
        MenuItem::new(5, "Tent", 0, TargetRule::Allies),
    ];
    hud
}

/// `view` after `keys` were pressed, dressed by `hud` at [`NOW`].
fn pressed(hud: &mut BattleHud, view: &BattleView, keys: &[MenuKey]) -> BattleView {
    for k in keys {
        hud.press(*k, view);
    }
    let mut dressed = view.clone();
    hud.decorate(&mut dressed, NOW);
    dressed
}

fn px(img: &[u8], w: u32, x: u32, y: u32) -> [u8; 3] {
    let i = ((y * w + x) * 4) as usize;
    [img[i], img[i + 1], img[i + 2]]
}

fn near(a: [u8; 3], b: [u8; 4], tol: u8) -> bool {
    (0..3).all(|i| a[i].abs_diff(b[i]) <= tol)
}

/// How many pixels in `x0..x1`, `y0..y1` (clipped to the picture) are within `tol` of `want`.
fn count_near(img: &[u8], w: u32, h: u32, (x0, y0, x1, y1): (i32, i32, i32, i32), want: [u8; 4], tol: u8) -> usize {
    let (x0, x1) = (x0.clamp(0, w as i32), x1.clamp(0, w as i32));
    let (y0, y1) = (y0.clamp(0, h as i32), y1.clamp(0, h as i32));
    (y0..y1).flat_map(|y| (x0..x1).map(move |x| (x, y))).filter(|&(x, y)| near(px(img, w, x as u32, y as u32), want, tol)).count()
}

/// A window of the layout in pixels: `x0, y0, x1, y1`.
fn pixels(l: &Layout, a: super::panels::Area) -> (i32, i32, i32, i32) {
    (a.x * l.s, a.y * l.s, (a.x + a.w) * l.s, (a.y + a.h) * l.s)
}

/// The bottom windows are on screen with their border and gradient, whatever else is drawn.
fn assert_windows(img: &[u8], w: u32, h: u32, l: &Layout, which: &[super::panels::Area]) {
    for a in which {
        let (x0, y0, _, y1) = pixels(l, *a);
        let mid = (y0 + y1) / 2;
        // The light border on the left edge, the gradient just inside it (darker towards the bottom).
        assert_eq!(px(img, w, (x0 + l.s / 2) as u32, mid as u32), [BORDER[0], BORDER[1], BORDER[2]], "{w}x{h}: border of {a:?}");
        let top = px(img, w, (x0 + 3 * l.s) as u32, (y0 + 3 * l.s) as u32);
        let bottom = px(img, w, (x0 + 3 * l.s) as u32, (y1 - 3 * l.s) as u32);
        assert!(near(top, PANEL_TOP, 40), "{w}x{h}: the top of {a:?} is the blue of the gradient: {top:?}");
        assert!(near(bottom, PANEL_BOTTOM, 40), "{w}x{h}: the bottom of {a:?} is the dark blue of the gradient: {bottom:?}");
    }
}

/// Pixels of text in the colour `want` somewhere in `area`.
fn text_pixels(img: &[u8], w: u32, h: u32, l: &Layout, a: super::panels::Area, want: [u8; 4]) -> usize {
    count_near(img, w, h, pixels(l, a), want, 6)
}

#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_battle_command_menu() {
    let Some(mut gpu) = Shooter::new() else { return };
    for (w, h) in SIZES {
        let l = layout(w, h, 3);
        let mut hud = hud();
        let view = pressed(&mut hud, &battle(), &[]);
        let img = gpu.shoot("01-command-menu", &view, w, h);
        assert_windows(&img, w, h, &l, &[l.enemy, l.command, l.party]);
        // The command window has its four words in the plain colour, the cursor's yellow pointer and the
        // greyed-out Defend (the engine has no such command yet) in a clearly different colour.
        assert!(text_pixels(&img, w, h, &l, l.command, [TEXT[0], TEXT[1], TEXT[2], 255]) > 20 * (l.s * l.s) as usize, "{w}x{h}: Attack is in the text colour");
        assert!(text_pixels(&img, w, h, &l, l.command, [ACCENT[0], ACCENT[1], ACCENT[2], 255]) > 8 * (l.s * l.s) as usize, "{w}x{h}: the pointer is yellow");
        assert!(text_pixels(&img, w, h, &l, l.command, DISABLED) > 20 * (l.s * l.s) as usize, "{w}x{h}: Defend is greyed out");
        // The party panel: our character in the accent colour, the HP and MP numbers, the ATB bars.
        assert!(text_pixels(&img, w, h, &l, l.party, [ACCENT[0], ACCENT[1], ACCENT[2], 255]) > 10 * (l.s * l.s) as usize, "{w}x{h}: our name is yellow");
        assert!(text_pixels(&img, w, h, &l, l.party, [INFO[0], INFO[1], INFO[2], 255]) > 10 * (l.s * l.s) as usize, "{w}x{h}: MP numbers are blue");
        assert!(text_pixels(&img, w, h, &l, l.party, [WARN[0], WARN[1], WARN[2], 255]) > 5 * (l.s * l.s) as usize, "{w}x{h}: Tifa's low HP is yellow");
        assert!(text_pixels(&img, w, h, &l, l.party, [255, 238, 120, 255]) > 30 * (l.s * l.s) as usize, "{w}x{h}: our full gauge is the bright one");
        assert!(text_pixels(&img, w, h, &l, l.party, [90, 150, 255, 255]) > 30 * (l.s * l.s) as usize, "{w}x{h}: the other gauges are blue");
        // The enemy names.
        assert!(text_pixels(&img, w, h, &l, l.enemy, [TEXT[0], TEXT[1], TEXT[2], 255]) > 30 * (l.s * l.s) as usize, "{w}x{h}: the enemy list has names");
        // The status line says whose turn it is.
        assert!(count_near(&img, w, h, (0, 14, w as i32, 14 + 12 * l.s), [ACCENT[0], ACCENT[1], ACCENT[2], 255], 6) > 100, "{w}x{h}: YOUR TURN in yellow under the band");
    }
}

#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_battle_magic_list() {
    let Some(mut gpu) = Shooter::new() else { return };
    for (w, h) in SIZES {
        let l = layout(w, h, 3);
        let mut hud = hud();
        // Down to Magic, open it, move the cursor to the third spell.
        let view = pressed(&mut hud, &battle(), &[MenuKey::Down, MenuKey::Confirm, MenuKey::Down, MenuKey::Down]);
        let img = gpu.shoot("02-magic-list", &view, w, h);
        assert_windows(&img, w, h, &l, &[l.left, l.party]);
        assert!(text_pixels(&img, w, h, &l, l.left, [ACCENT[0], ACCENT[1], ACCENT[2], 255]) > 20 * (l.s * l.s) as usize, "{w}x{h}: the title and pointer are yellow");
        assert!(text_pixels(&img, w, h, &l, l.left, [TEXT[0], TEXT[1], TEXT[2], 255]) > 10 * (l.s * l.s) as usize, "{w}x{h}: the highlighted spell is bright");
        assert!(text_pixels(&img, w, h, &l, l.left, [200, 208, 240, 255]) > 10 * (l.s * l.s) as usize, "{w}x{h}: the other spells are the soft colour");
        // The list covers the enemy list and the command window: nothing of theirs is left under it.
        assert!(text_pixels(&img, w, h, &l, l.command, DISABLED) < 8 * (l.s * l.s) as usize, "{w}x{h}: no greyed Defend: the list is on top");
        // The status line explains the keys (or why a spell cannot be cast).
        assert!(count_near(&img, w, h, (0, 14, w as i32, 14 + 12 * l.s), [ACCENT[0], ACCENT[1], ACCENT[2], 255], 6) > 100);
        // A spell that costs more than we have is greyed out and says so up top: move to Meteor (the last).
        let mut hud = self::hud();
        let view = pressed(&mut hud, &battle(), &[MenuKey::Down, MenuKey::Confirm, MenuKey::Up]);
        let img = gpu.shoot("03-magic-list-meteor", &view, w, h);
        assert!(text_pixels(&img, w, h, &l, l.left, DISABLED) > 20 * (l.s * l.s) as usize, "{w}x{h}: Meteor and Fire 2's neighbours are greyed out");
        assert!(count_near(&img, w, h, (0, 14, w as i32, 14 + 12 * l.s), [WARN[0], WARN[1], WARN[2], 255], 6) > 100, "{w}x{h}: 'Not enough MP' in the warning colour under the band");
    }
}

#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_battle_item_list() {
    let Some(mut gpu) = Shooter::new() else { return };
    for (w, h) in SIZES {
        let l = layout(w, h, 3);
        let mut hud = hud();
        let view = pressed(&mut hud, &battle(), &[MenuKey::Down, MenuKey::Down, MenuKey::Confirm, MenuKey::Down]);
        let img = gpu.shoot("04-item-list", &view, w, h);
        assert_windows(&img, w, h, &l, &[l.left, l.party]);
        assert!(text_pixels(&img, w, h, &l, l.left, [ACCENT[0], ACCENT[1], ACCENT[2], 255]) > 20 * (l.s * l.s) as usize, "{w}x{h}: the title is yellow");
        assert!(text_pixels(&img, w, h, &l, l.left, [TEXT[0], TEXT[1], TEXT[2], 255]) > 10 * (l.s * l.s) as usize);
        assert!(text_pixels(&img, w, h, &l, l.left, [200, 208, 240, 255]) > 10 * (l.s * l.s) as usize);
    }
}

#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_battle_target_selection() {
    let Some(mut gpu) = Shooter::new() else { return };
    for (w, h) in SIZES {
        let l = layout(w, h, 3);
        // Attack, then one enemy down: the pointer is over the second enemy, whose name is yellow.
        let mut hud = hud();
        let view = pressed(&mut hud, &battle(), &[MenuKey::Confirm, MenuKey::Down]);
        let img = gpu.shoot("05-target-enemy", &view, w, h);
        let (ax, ay) = flat_anchors(&view, w, h).of(&view, ActorId(4)).expect("an anchor");
        let cell = 8 * (l.s + 1);
        let around = (ax as i32 - cell, ay as i32 - cell, ax as i32 + cell, ay as i32 + 4);
        assert!(count_near(&img, w, h, around, [ACCENT[0], ACCENT[1], ACCENT[2], 255], 6) > 20 * (l.s * l.s) as usize, "{w}x{h}: a yellow pointer over the aimed enemy");
        let elsewhere = (ax as i32 - 2 * cell - 200, ay as i32 - cell, ax as i32 - 120, ay as i32 + 4);
        assert_eq!(count_near(&img, w, h, elsewhere, [ACCENT[0], ACCENT[1], ACCENT[2], 255], 6), 0, "{w}x{h}: and only there");
        assert_windows(&img, w, h, &l, &[l.enemy, l.party]);
        assert!(text_pixels(&img, w, h, &l, l.enemy, [ACCENT[0], ACCENT[1], ACCENT[2], 255]) > 15 * (l.s * l.s) as usize, "{w}x{h}: the aimed enemy's name is yellow in the list");
        assert!(text_pixels(&img, w, h, &l, l.command, [TEXT[0], TEXT[1], TEXT[2], 255]) < 5 * (l.s * l.s) as usize, "{w}x{h}: the command window is gone while aiming");

        // A healing spell aims at the party: Magic, down to Cure, one ally right.
        let mut hud = self::hud();
        let view = pressed(&mut hud, &battle(), &[MenuKey::Down, MenuKey::Confirm, MenuKey::Down, MenuKey::Down, MenuKey::Down, MenuKey::Confirm, MenuKey::Right]);
        let img = gpu.shoot("06-target-ally", &view, w, h);
        let (ax, ay) = flat_anchors(&view, w, h).of(&view, ActorId(1)).expect("an anchor");
        let around = (ax as i32 - cell, ay as i32 - cell, ax as i32 + cell, ay as i32 + 4);
        assert!(count_near(&img, w, h, around, [ACCENT[0], ACCENT[1], ACCENT[2], 255], 6) > 20 * (l.s * l.s) as usize, "{w}x{h}: the pointer is over Barret");
        assert!(text_pixels(&img, w, h, &l, l.party, [ACCENT[0], ACCENT[1], ACCENT[2], 255]) > 15 * (l.s * l.s) as usize, "{w}x{h}: and his row in the party panel is marked");
    }
}

#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_battle_damage_popups_and_the_action_banner() {
    let Some(mut gpu) = Shooter::new() else { return };
    for (w, h) in SIZES {
        let l = layout(w, h, 3);
        let mut hud = hud();
        // Real events through the hud: a hit, a critical hit and a miss; a heal and restored MP are added
        // to the view by hand (the engine's events for them do not exist yet).
        // (They arrive early enough that their blows have been landing for 300 ms: a number is born at the
        // impact, see `scene::cue`.)
        hud.on_events(
            NOW - 0.3 - super::scene::cue::impact_delay(None),
            &[
                BattleEvent::Damage { actor: ActorId(1), target: ActorId(3), amount: 1234, critical: false },
                BattleEvent::Damage { actor: ActorId(1), target: ActorId(5), amount: 9999, critical: true },
                BattleEvent::Miss { actor: ActorId(5), target: ActorId(2) },
            ],
            false,
        );
        hud.show_banner(NOW - 0.1, "Fire".to_string());
        let mut view = battle();
        view.party[1].ready = false;
        view.party[1].gauge = 20_000;
        hud.decorate(&mut view, NOW);
        view.popups.push(Popup { target: ActorId(0), kind: PopupKind::Heal { amount: 300 }, age_ms: 300 });
        view.popups.push(Popup { target: ActorId(1), kind: PopupKind::Mp { amount: 30 }, age_ms: 300 });
        view.popups.push(Popup { target: ActorId(4), kind: PopupKind::Damage { amount: 77, critical: false }, age_ms: 300 });
        view.popups.push(Popup { target: ActorId(4), kind: PopupKind::Damage { amount: 12, critical: false }, age_ms: 150 });
        assert_eq!(view.popups.len(), 7);
        assert!(view.banner.as_ref().is_some_and(|b| b == &Banner { text: "Fire".into(), age_ms: 100 }));
        let img = gpu.shoot("07-damage-popups-and-banner", &view, w, h);
        let anchors = flat_anchors(&view, w, h);
        let cell = 8 * (l.s + 1);
        let over = |actor: u8| {
            let (ax, ay) = anchors.of(&view, ActorId(actor)).expect("an anchor");
            (ax as i32 - 6 * cell, ay as i32 - 8 * cell, ax as i32 + 6 * cell, ay as i32)
        };
        let white = [255, 255, 255, 255];
        assert!(count_near(&img, w, h, over(3), white, 0) > 40, "{w}x{h}: 1234 in white over the first enemy");
        assert!(count_near(&img, w, h, over(5), [ACCENT[0], ACCENT[1], ACCENT[2], 255], 0) > 40, "{w}x{h}: the critical 9999 is gold");
        assert!(count_near(&img, w, h, over(0), [112, 236, 132, 255], 0) > 40, "{w}x{h}: the heal is green");
        assert!(count_near(&img, w, h, over(1), [130, 210, 255, 255], 0) > 40, "{w}x{h}: restored MP is blue");
        assert!(count_near(&img, w, h, over(2), [214, 218, 236, 255], 0) > 40, "{w}x{h}: 'Miss' over Tifa");
        // The banner: a window at the top with the action's name.
        let banner = super::panels::Area { x: (l.cw - 56) / 2, y: l.banner_y, w: 56, h: 16 };
        assert!(count_near(&img, w, h, pixels(&l, banner), [TEXT[0], TEXT[1], TEXT[2], 255], 6) > 10 * (l.s * l.s) as usize, "{w}x{h}: 'Fire' in the banner");
        assert!(count_near(&img, w, h, pixels(&l, banner), [BORDER[0], BORDER[1], BORDER[2], 255], 0) > 20, "{w}x{h}: in a bordered window");
        // Not our turn: no command window, no yellow pointer.
        assert!(text_pixels(&img, w, h, &l, l.command, [TEXT[0], TEXT[1], TEXT[2], 255]) < 5 * (l.s * l.s) as usize);
        // A popup near the end of its life is half gone: the same view a moment later has fewer white pixels.
        let mut late = view.clone();
        for p in &mut late.popups {
            p.age_ms = 1000;
        }
        let img_late = gpu.shoot("08-damage-popups-fading", &late, w, h);
        assert!(count_near(&img_late, w, h, over(3), white, 0) < count_near(&img, w, h, over(3), white, 0) / 2, "{w}x{h}: it has faded");
    }
}

fn ended_view(outcome: Outcome) -> (BattleHud, BattleView) {
    let mut hud = hud();
    let mut last = battle();
    last.party[1].ready = false;
    if outcome == Outcome::Victory {
        for e in &mut last.enemies {
            e.hp = 0;
            e.alive = false;
        }
    } else {
        for p in &mut last.party {
            p.hp = 0;
            p.alive = false;
            p.ready = false;
        }
    }
    let reward = RewardLine {
        exp: 626,
        gil: 1_200,
        ap: 18,
        items: vec![RewardItem { name: "Potion".into(), count: 2 }, RewardItem { name: "Ether".into(), count: 1 }],
    };
    hud.finish(NOW, last.clone(), outcome, (outcome == Outcome::Victory).then_some(reward));
    (hud, last)
}

#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_battle_victory_and_defeat_panels() {
    let Some(mut gpu) = Shooter::new() else { return };
    for (w, h) in SIZES {
        let l = layout(w, h, 3);
        let (hud, _) = ended_view(Outcome::Victory);
        // Just before the panel's moment: the last view and the status line, no panel yet.
        let early = hud.linger_view(NOW + f64::from(PANEL_DELAY_MS) / 1000.0 - 0.2).expect("lingering");
        let img_early = gpu.shoot("09-victory-before-the-panel", &early, w, h);
        // The strip of the stage under the banner, across the window: the panel hangs from the top of it, over the
        // left (where the beaten enemies were) when the party stands on the right, as it does on this stage.
        let panel_area = (0, l.banner_y + 20, l.cw, l.enemy.y - 4);
        let party_side = (l.cw / 2 + 20, l.banner_y + 20, l.cw, l.enemy.y - 4);
        let in_px = |a: (i32, i32, i32, i32)| (a.0 * l.s, a.1 * l.s, a.2 * l.s, a.3 * l.s);
        let gold = [ACCENT[0], ACCENT[1], ACCENT[2], 255];
        let before = count_near(&img_early, w, h, (panel_area.0 * l.s, panel_area.1 * l.s, panel_area.2 * l.s, panel_area.3 * l.s), gold, 6);
        // The panel itself, with a key allowed to close it.
        let view = hud.linger_view(NOW + 2.0).expect("lingering");
        assert!(view.finale.as_ref().is_some_and(|f| f.dismissible));
        let img = gpu.shoot("10-victory-panel", &view, w, h);
        let after = count_near(&img, w, h, (panel_area.0 * l.s, panel_area.1 * l.s, panel_area.2 * l.s, panel_area.3 * l.s), gold, 6);
        assert!(after > before + 40 * (l.s * l.s) as usize, "{w}x{h}: the gold VICTORY title appears with the panel ({before} -> {after})");
        assert_eq!(
            count_near(&img, w, h, in_px(party_side), gold, 6),
            count_near(&img_early, w, h, in_px(party_side), gold, 6),
            "{w}x{h}: the panel adds nothing over the side the party stands on"
        );
        let border = count_near(&img, w, h, (panel_area.0 * l.s, panel_area.1 * l.s, panel_area.2 * l.s, panel_area.3 * l.s), [BORDER[0], BORDER[1], BORDER[2], 255], 0);
        assert!(border > 30, "{w}x{h}: in a bordered window");
        let white = count_near(&img, w, h, (panel_area.0 * l.s, panel_area.1 * l.s, panel_area.2 * l.s, panel_area.3 * l.s), [TEXT[0], TEXT[1], TEXT[2], 255], 6);
        assert!(white > 40 * (l.s * l.s) as usize, "{w}x{h}: EXP, Gil and AP lines ({white})");
        // The party panel is still there under it, and the dead enemies are not listed.
        assert_windows(&img, w, h, &l, &[l.party]);

        let (hud, _) = ended_view(Outcome::Defeat);
        let view = hud.linger_view(NOW + 2.0).expect("lingering");
        let img = gpu.shoot("11-defeat-panel", &view, w, h);
        let red = count_near(&img, w, h, (panel_area.0 * l.s, panel_area.1 * l.s, panel_area.2 * l.s, panel_area.3 * l.s), [ERROR[0], ERROR[1], ERROR[2], 255], 6);
        assert!(red > 40 * (l.s * l.s) as usize, "{w}x{h}: the DEFEAT title is red ({red})");
    }
}

#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_battle_waiting_for_the_gauge_and_the_status_band() {
    let Some(mut gpu) = Shooter::new() else { return };
    for (w, h) in SIZES {
        let l = layout(w, h, 3);
        let mut view = battle();
        view.party[1].ready = false;
        view.party[1].gauge = 33_000;
        let img = gpu.shoot("12-waiting-for-the-gauge", &view, w, h);
        assert_eq!(px(&img, w, 5, 5), [40, 150, 80], "{w}x{h}: the green band across the top");
        assert_windows(&img, w, h, &l, &[l.enemy, l.party]);
        assert!(text_pixels(&img, w, h, &l, l.command, [TEXT[0], TEXT[1], TEXT[2], 255]) < 5 * (l.s * l.s) as usize, "{w}x{h}: no command window");
        // A diverged replay: the band is red and the line says so.
        view.status = Status::Diverged;
        let img = gpu.shoot("13-diverged", &view, w, h);
        assert_eq!(px(&img, w, 5, 5), [220, 30, 30], "{w}x{h}: the red band");
        assert!(count_near(&img, w, h, (0, 14, w as i32, 14 + 12 * l.s), [255, 122, 122, 255], 6) > 100, "{w}x{h}: BATTLE DIVERGED in red");
        // A machine with no battle data: the grey band, the warning, and no windows.
        let img = gpu.shoot("14-no-battle-data", &BattleView::unverified(BattleId(1)), w, h);
        assert_eq!(px(&img, w, 5, 5), [120, 120, 130], "{w}x{h}: the grey band");
    }
}

/// Protocol v7: Barret's limit gauge is full, so the first command says Limit where it said Attack, and the party
/// panel draws a thin limit bar under each ATB bar: orange while it fills (Cloud's and Tifa's), lit when it is full
/// (Barret's). Defend and Escape are given, so nothing in the command window is greyed out.
#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_battle_limit_ready_menu_and_the_limit_bars() {
    let Some(mut gpu) = Shooter::new() else { return };
    let orange = [LIMIT_RGB[0], LIMIT_RGB[1], LIMIT_RGB[2], 255];
    let lit = [LIMIT_FULL_RGB[0], LIMIT_FULL_RGB[1], LIMIT_FULL_RGB[2], 255];
    for (w, h) in SIZES {
        let l = layout(w, h, 3);
        let unit = (l.s * l.s) as usize;
        let mut view = battle();
        view.party[0].limit = 100;
        view.party[1].limit = 255;
        view.party[2].limit = 200;
        let mut hud = hud();
        let model = hud.model_mut();
        model.defend = true;
        model.escape = true;
        model.limit = Some(MenuLimit::new(135, "Big Shot", TargetRule::Enemies, true));
        let dressed = pressed(&mut hud, &view, &[]);
        let commands: Vec<&str> = dressed.menu.as_ref().expect("a menu").commands.iter().map(|c| c.label).collect();
        assert_eq!(commands, ["Limit", "Magic", "Item", "Defend", "Escape"], "{w}x{h}");
        let img = gpu.shoot("15-limit-ready", &dressed, w, h);
        assert_windows(&img, w, h, &l, &[l.enemy, l.command, l.party]);
        assert!(text_pixels(&img, w, h, &l, l.party, lit) > 40 * unit, "{w}x{h}: Barret's full limit bar is lit");
        assert!(text_pixels(&img, w, h, &l, l.party, orange) > 40 * unit, "{w}x{h}: the others are filling in orange");
        assert!(text_pixels(&img, w, h, &l, l.command, DISABLED) < 5 * unit, "{w}x{h}: nothing in the command window is greyed out");
        // The same battle with empty gauges draws no limit bar at all (a character without a limit shows nothing).
        let mut empty = battle();
        empty.party.iter_mut().for_each(|b| b.limit = 0);
        let mut hud = self::hud();
        let img = gpu.shoot("15b-no-limit-bars", &pressed(&mut hud, &empty, &[]), w, h);
        assert_eq!(text_pixels(&img, w, h, &l, l.party, orange) + text_pixels(&img, w, h, &l, l.party, lit), 0, "{w}x{h}: no bar without a gauge");
        // The limit is chosen like an attack: Confirm on the first row aims at an enemy.
        let mut hud = self::hud();
        {
            let model = hud.model_mut();
            model.defend = true;
            model.escape = true;
            model.limit = Some(MenuLimit::new(135, "Big Shot", TargetRule::Enemies, true));
        }
        let aiming = pressed(&mut hud, &view, &[MenuKey::Confirm, MenuKey::Down]);
        assert!(aiming.menu.as_ref().is_some_and(|m| m.aim.is_some()), "{w}x{h}: the limit aims at an enemy");
        gpu.shoot("16-limit-target", &aiming, w, h);
    }
}

/// Protocol v7: the end-of-battle panel after an escape says so, in the info colour, and pays nothing.
#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_battle_escaped_panel() {
    let Some(mut gpu) = Shooter::new() else { return };
    for (w, h) in SIZES {
        let l = layout(w, h, 3);
        let mut hud = hud();
        let last = battle();
        hud.finish(NOW, last, Outcome::Escaped, None);
        let view = hud.linger_view(NOW + 2.0).expect("lingering");
        let img = gpu.shoot("17-escaped-panel", &view, w, h);
        let strip = (0, (l.banner_y + 20) * l.s, l.cw * l.s, (l.enemy.y - 4) * l.s);
        let blue = [INFO[0], INFO[1], INFO[2], 255];
        assert!(count_near(&img, w, h, strip, blue, 6) > 40 * (l.s * l.s) as usize, "{w}x{h}: the ESCAPED title is in the info colour");
        assert_windows(&img, w, h, &l, &[l.party]);
    }
}
