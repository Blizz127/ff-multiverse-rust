//! Dialogue windows over the owner's REAL fields: pictures to look at, and a check that nothing the
//! windows would draw is garbage. They need the owner's data (`FMV_FF7_LIBRARY`, `FMV_FF7_DATA` or
//! `FMV_FF7_FLEVEL`, as the avatar shots do) and, for the pictures, a GPU or a software Vulkan/GL
//! driver, so they only run on request:
//!
//! ```text
//! FMV_FF7_DATA=<.../data> FMV_SHOT_DIR=<dir> cargo test -p fmv-client --bin fmv-client \
//!     -- --ignored dialogue --test-threads=1
//! ```
//!
//! * `real_dialogs_fold_to_text_the_font_draws`: every dialog of `md1stin` and `md1_1` (and, with
//!   `FMV_FF7_ALL_FIELDS` set, of all 702 fields), through the same tokenizer and fold the window
//!   uses, draws nothing but printable ASCII and no `{XX}` marker survives.
//! * `offscreen_real_ff7_dialogue_windows`: `dialogue-*.png`, the window over the real field with its
//!   characters: the guard of `md1stin` as the talk key opens it, the lines the AVALANCHE members
//!   speak in `md1_1` (nobody there has talk text at the start of the game, so these come from the
//!   cutscene scripts the same walker runs), and a real question with its pointer.

use super::layout::fold;
use super::*;
use crate::avatar::library::AvatarLibrary;
use crate::avatar::source::LgpLoader;
use crate::avatar::Avatars;
use crate::backdrop::FieldView;
use crate::ff7::{prewarm, resolve_char_lgp, resolve_flevel, Ff7Labels, Ff7Source};
use crate::render::Scene;
use crate::session::SessionInfo;
use crate::state::ClientState;
use crate::ui::hud::{self, Subject};
use crate::ui::testkit::{save, Offscreen};
use crate::window::{field_draw, label_anchor};
use fmv_adapter_api::AreaRuntime;
use fmv_core::{AccountId, AnimState, AreaId, AvatarAppearance, EntityId, EntitySnapshot, GameId};
use fmv_ff7::script::dialogue::{parse_message, Piece, StartOfGame, StoryVars, TalkSession};
use fmv_ff7::script::FieldScript;
use fmv_ff7::Ff7Adapter;
use fmv_protocol::{NpcSnapshot, ServerMsg, NPC_NAME_CHARS, NPC_REF_CHARS};
use std::path::PathBuf;
use std::sync::Arc;

/// The owner's FF7 data, opened read-only: the field adapter and the character archive.
struct Real {
    adapter: Arc<Ff7Adapter>,
    char_lgp: PathBuf,
}

/// `FMV_FF7_LIBRARY`, else `FMV_FF7_DATA`, else `FMV_FF7_FLEVEL`; `None` (with a note) when none is set.
fn open() -> Option<Real> {
    let direct = |path: PathBuf| Ff7Source::Flevel { path, labels: Ff7Labels::default() };
    let source = if let Some(dir) = std::env::var_os("FMV_FF7_LIBRARY") {
        Ff7Source::Library { state_dir: Some(dir.into()), set: None }
    } else if let Some(dir) = std::env::var_os("FMV_FF7_DATA") {
        direct(PathBuf::from(dir).join("field").join("flevel.lgp"))
    } else if let Some(path) = std::env::var_os("FMV_FF7_FLEVEL") {
        direct(path.into())
    } else {
        eprintln!("skipped: none of FMV_FF7_LIBRARY, FMV_FF7_DATA, FMV_FF7_FLEVEL is set");
        return None;
    };
    let resolved = resolve_flevel(&source).expect("the field archive is found");
    let adapter = Ff7Adapter::from_flevel_path(&resolved.path, resolved.provenance).expect("the field archive opens");
    prewarm(&adapter);
    let char_lgp = resolve_char_lgp(&source).expect("the character archive is looked for").expect("a char.lgp next to the field archive");
    Some(Real { adapter: Arc::new(adapter), char_lgp })
}

fn section_of(real: &Real, key: &str) -> Vec<u8> {
    let area = real.adapter.load_field(key).expect("field");
    FieldScript::from_field(area.field_file()).expect("script").bytes().to_vec()
}

#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA, FMV_FF7_FLEVEL or FMV_FF7_LIBRARY)"]
fn real_dialogs_fold_to_text_the_font_draws() {
    let Some(real) = open() else { return };
    let keys: Vec<String> = if std::env::var_os("FMV_FF7_ALL_FIELDS").is_some() {
        use fmv_adapter_api::GameAdapter;
        real.adapter.list_areas().into_iter().map(|a| a.id.key).collect()
    } else {
        vec!["md1stin".into(), "md1_1".into()]
    };
    let (mut dialogs, mut chars) = (0usize, 0usize);
    for key in &keys {
        let section = section_of(&real, key);
        let script = FieldScript::parse(&section).expect("script");
        for d in script.dialogs() {
            let raw = d.offset.checked_add(d.len).and_then(|end| script.bytes().get(d.offset..end)).expect("dialog bytes");
            dialogs += 1;
            for piece in parse_message(raw, &StartOfGame) {
                if let Piece::Text(t) = piece {
                    let folded = fold(&t);
                    chars += folded.chars().count();
                    assert!(folded.chars().all(crate::ui::font::is_drawable), "{key}: not drawable: {folded:?}");
                    assert!(!folded.contains('{') || !folded.contains('}'), "{key}: a code marker survived: {folded:?}");
                    // A character the fold has no stand-in for would come out as a question mark.
                    for c in t.chars().filter(|&c| c != '?') {
                        assert_ne!(fold(&c.to_string()), "?", "{key}: {c:?} has no stand-in in the font");
                    }
                }
            }
        }
    }
    eprintln!("{} field(s), {dialogs} dialogs, {chars} characters: all drawable, no marker, no question mark", keys.len());
    assert!(dialogs >= 114 + 43);
}

/// What the story says: the progress word (`Var[2][0]`, bytes 0 and 1 of bank 2) and nothing else.
struct Progress(u16);

impl StoryVars for Progress {
    fn byte(&self, bank: u8, address: u8) -> u8 {
        match (bank, address) {
            (2, 0) => self.0.to_le_bytes()[0],
            (2, 1) => self.0.to_le_bytes()[1],
            _ => 0,
        }
    }

    fn party_member(&self, slot: u8) -> Option<u8> {
        StartOfGame.party_member(slot)
    }
}

/// A real field with its characters as the window draws them, and the local player where the shot wants it.
struct Stage {
    view: FieldView,
    state: ClientState,
    avatars: Avatars,
    key: String,
    own_tint: [u8; 3],
}

/// The field `key` with all its announced characters and the local player (Cloud) standing `from_npc`
/// units from the character called `near`, facing it (or on the first spawn point with no `near`).
fn stage(real: &Real, key: &str, near: Option<(&str, f32)>) -> Stage {
    let view = FieldView::ff7_with(&real.adapter, key, true).expect("field view");
    let area = real.adapter.load_field(key).expect("field");
    let clip = |text: &str, max: usize| -> String { text.chars().filter(|c| !c.is_control()).take(max).collect() };
    let announced: Vec<NpcSnapshot> = area
        .npcs()
        .iter()
        .filter(|n| n.unconditional && n.pos.is_finite() && n.facing.is_finite())
        .enumerate()
        .map(|(i, n)| NpcSnapshot {
            id: u32::try_from(i).unwrap_or(u32::MAX),
            name: clip(&n.entity_name, NPC_NAME_CHARS),
            model: clip(&n.appearance.model, NPC_REF_CHARS),
            idle_animation: n.appearance.idle_animation.as_deref().map(|a| clip(a, NPC_REF_CHARS)),
            pos: n.pos,
            facing: n.facing,
        })
        .collect();
    let target = near.and_then(|(name, from)| announced.iter().find(|n| n.name == name).map(|n| (n.pos, from)));
    // Stand `from_npc` units from the character on the side the area accepts, facing it.
    let (own_pos, facing) = match target {
        Some((t, from_npc)) => {
            let ring = [(0.0, -1.0), (1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.7, -0.7), (-0.7, -0.7), (0.7, 0.7), (-0.7, 0.7)];
            let placed = ring
                .iter()
                .filter_map(|(dx, dy)| {
                    let want = Pos::new(t.x + dx * from_npc, t.y + dy * from_npc, t.z);
                    let p = area.place_arrival(want, None);
                    (p.is_finite() && p.dist_xy(want) < 1.0).then_some(p)
                })
                .next()
                .expect("a place beside the character");
            (placed, (t.y - placed.y).atan2(t.x - placed.x))
        }
        None => (area.spawn_points()[0], 0.0),
    };
    let area_id = AreaId { game: GameId::Ff7, key: key.to_string() };
    let own_tint = [90, 125, 245];
    let mut state = ClientState::new(EntityId(1), AccountId(1), own_pos, area_id.clone());
    state.change_area(area_id.clone(), own_pos, facing, false);
    let me = EntitySnapshot {
        id: EntityId(1),
        pos: own_pos,
        facing,
        anim: AnimState::Idle,
        avatar: AvatarAppearance::new("Cloud", own_tint, Some("ff7/cloud".to_string())),
    };
    assert!(state.apply(0.0, ServerMsg::AreaSnapshot { area: area_id, tick: 1, entities: vec![me], npcs: announced }).is_none());
    let avatars = Avatars::new(AvatarLibrary::new(Box::new(LgpLoader::new(&real.char_lgp))));
    Stage { view, state, avatars, key: key.to_string(), own_tint }
}

impl Stage {
    fn around(&self) -> Around<'_> {
        Around { area: &self.key, own: self.state.own.pos(), facing: self.state.own.facing(), npcs: self.state.npc_views(), blocked: false }
    }

    /// The field as the window draws it, with `dialogue` over it, saved as `<name>.png`.
    fn shoot(&mut self, gpu: &mut Offscreen, name: &str, w: u32, h: u32, dialogue: impl Fn(u32, u32, &mut DrawList)) -> Vec<u8> {
        let (mut epoch, render_t) = (0, 1.0);
        // The first frame loads the models the area names; the second is the steady state.
        field_draw(Some(&mut self.avatars), &mut epoch, &self.view, &self.state, self.own_tint, render_t, 0.0);
        let draw = field_draw(Some(&mut self.avatars), &mut epoch, &self.view, &self.state, self.own_tint, render_t, 0.0);
        let built = draw.built.as_ref().expect("the models were built");
        let scene = Scene { bounds: self.view.bounds, triangles: &[], players: &draw.markers, fixture_banner: false, backdrop: self.view.backdrop.as_ref() };
        let info = SessionInfo {
            name: "Cloud".into(),
            tint: self.own_tint,
            area_id: AreaId { game: GameId::Ff7, key: self.key.clone() },
            fixture: false,
        };
        let anchor = |subject: Subject, at: Pos| label_anchor(draw.built.as_ref(), &self.view, draw.own_who, subject, at);
        let hud_input = hud::gather_anchored(&self.state, &info, &self.view, render_t, &anchor);
        let mut overlay = DrawList::new();
        hud::draw_hud(&hud_input, w, h, &mut overlay);
        dialogue(w, h, &mut overlay);
        let img = gpu.render_field(w, h, &scene, Some(&built.frame), &overlay);
        save(name, w, h, &img);
        img
    }
}

/// How many pixels of `img` are the window's light border colour or brighter text white (so a shot
/// that came out empty is caught).
fn bright(img: &[u8]) -> usize {
    img.chunks(4).filter(|p| p[0] > 230 && p[1] > 230 && p[2] > 240).count()
}

#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_DATA, FMV_FF7_FLEVEL or FMV_FF7_LIBRARY) and a GPU or software Vulkan/GL driver"]
fn offscreen_real_ff7_dialogue_windows() {
    let Some(real) = open() else { return };
    let Some(mut gpu) = Offscreen::new() else { return };
    let (w, h) = (1100, 680);

    // 1. md1stin: the guard on the platform, as the talk key opens it.
    let mut platform = stage(&real, "md1stin", Some(("gu0", 45.0)));
    let mut talk = Talk::ff7(Some(Arc::clone(&real.adapter)));
    let before = platform.shoot(&mut gpu, "dialogue-00-md1stin-before", w, h, |_, _, _| {});
    let opened = talk.key(KeyCode::Enter, true, false, &platform.around());
    assert!(opened && talk.is_open(), "Enter beside the guard opens its dialogue");
    assert_eq!(talk.message().map(|m| m.origin.is_canon()), Some(true));
    let with = platform.shoot(&mut gpu, "dialogue-01-md1stin-guard", w, h, |w, h, out| talk.draw(w, h, 0.0, out));
    assert!(bright(&with) > bright(&before) + 200, "the window put light text on the picture");
    let big = platform.shoot(&mut gpu, "dialogue-02-md1stin-guard-1920x1080", 1920, 1080, |w, h, out| talk.draw(w, h, 0.0, out));
    assert!(bright(&big) > 0);
    // Away from the guard (behind it) nothing opens.
    let mut far = Talk::ff7(Some(Arc::clone(&real.adapter)));
    let around = Around { own: Pos::new(0.0, 0.0, 0.0), ..platform.around() };
    assert!(!far.key(KeyCode::Enter, true, false, &around), "nobody near");
    assert!(talk.key(KeyCode::Enter, true, false, &platform.around()));
    assert!(!talk.is_open(), "one window: the next Enter closes it");

    // 2. md1_1: nobody has talk text at the start of the game; the AVALANCHE members speak in the
    // scripts the director starts. The same walker and window show them: Biggs's first lines, then
    // the exchange of script 4, whose third window is Cloud's (requested from Biggs's script).
    let section = section_of(&real, "md1_1");
    let script = FieldScript::parse(&section).expect("script");
    let (av_m, av_l) = (script.entity_by_name("av_m").expect("Biggs"), script.entity_by_name("av_l").expect("Jessie"));
    let mut hall = stage(&real, "md1_1", Some(("av_m", 90.0)));
    let pages = |slot: usize, entity: usize| -> DialogueWindow {
        let session = TalkSession::script(section.clone(), entity, slot, Box::new(StartOfGame), 1).expect("session");
        DialogueWindow::open(Box::new(session)).expect("something to say")
    };
    let window = pages(3, av_m);
    hall.shoot(&mut gpu, "dialogue-10-md1_1-biggs", w, h, |w, h, out| window.draw(w, h, 0.0, out));
    let window = pages(3, av_l);
    hall.shoot(&mut gpu, "dialogue-11-md1_1-jessie", w, h, |w, h, out| window.draw(w, h, 0.0, out));
    let mut window = pages(4, av_m);
    let mut n = 0;
    loop {
        // (the pictures are of the page on show; Enter turns to the next page or window)
        let shot = format!("dialogue-12-md1_1-exchange-{n}");
        hall.shoot(&mut gpu, &shot, w, h, |w, h, out| window.draw(w, h, 0.0, out));
        n += 1;
        if !window.key(DialogueKey::Confirm, (w, h)) || n >= 8 {
            break;
        }
    }
    eprintln!("the md1_1 exchange of script 4 took {n} page(s) over the windows");
    assert!(n >= 4, "four windows at least: Biggs, Biggs, Cloud, Biggs");

    // 3. A real question: the Big Wheel's attendant at the start of the game (any progress below 677
    // except 598 has him ask whether to ride), with the pointer on the first answer and then moved
    // down; and what the same attendant says once the story hook says the game is further on.
    let section = section_of(&real, "bigwheel");
    let script = FieldScript::parse(&section).expect("script");
    let staff = script.entity_by_name("staff").expect("the attendant");
    let mut wheel = stage(&real, "bigwheel", None);
    let session = TalkSession::script(section.clone(), staff, 1, Box::new(StartOfGame), 1).expect("session");
    let mut window = DialogueWindow::open(Box::new(session)).expect("something to say");
    // His greeting is a page of its own before the question (and it may be a window of its own).
    let mut asked = false;
    for n in 0..6 {
        let asking = window.on_choices((w, h));
        let shot = format!("dialogue-20-bigwheel-{n}{}", if asking { "-question" } else { "" });
        wheel.shoot(&mut gpu, &shot, w, h, |w, h, out| window.draw(w, h, 0.0, out));
        if asking {
            asked = true;
            assert_eq!(window.position().1, 0, "the pointer starts on the first answer");
            assert!(window.key(DialogueKey::Down, (w, h)));
            assert_eq!(window.position().1, 1);
            wheel.shoot(&mut gpu, "dialogue-21-bigwheel-pointer-moved", w, h, |w, h, out| window.draw(w, h, 0.0, out));
            break;
        }
        assert!(window.key(DialogueKey::Confirm, (w, h)), "the question comes before the end");
    }
    assert!(asked, "a question was reached");
    let session = TalkSession::script(section, staff, 1, Box::new(Progress(700)), 1).expect("session");
    let window = DialogueWindow::open(Box::new(session)).expect("something to say");
    assert!(window.message().choice.is_none(), "further on, the attendant says something else and asks nothing");
    wheel.shoot(&mut gpu, "dialogue-22-bigwheel-progress-700", w, h, |w, h, out| window.draw(w, h, 0.0, out));
}
