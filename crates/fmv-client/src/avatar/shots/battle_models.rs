//! Pictures and checks of the FF7 *battle* models (enemies, battle locations, the playable
//! characters), loaded by `fmv_ff7::battle_model` from the owner's own `battle.lgp` and drawn
//! through the real avatar renderer (`AvatarFrame` -> `Gpu::draw_field`) into an offscreen texture,
//! read back and written as PNGs to look at.
//!
//! Everything here is data-gated and `#[ignore]`d: it needs the owner's FF7 data (`FMV_FF7_LIBRARY`,
//! `FMV_FF7_DATA` or `FMV_FF7_FLEVEL`, as the other real-data shots) and, for the pictures, a GPU or
//! a software Vulkan/GL driver (lavapipe/llvmpipe):
//!
//! ```text
//! FMV_FF7_DATA=<.../data> cargo test -p fmv-client -- --ignored offscreen_battle --nocapture
//! FMV_FF7_DATA=<.../data> cargo test -p fmv-client -- --ignored real_battle --nocapture
//! ```
//!
//! The PNGs go to `$FMV_SHOT_DIR`, default `~/.cache/claude-tmp/fmv-shots/sw-bmodel`.
//!
//! * `real_battle_ids_and_codes_agree_with_scene_bin` (no GPU) checks the loader's mappings against
//!   `scene.bin` and the field archive: every enemy id of every formation names an enemy model, how
//!   the enemies' animation indices land on the animation slots, every battle location code names a
//!   location, and which location the fields of the first mission and of a few distinctive places
//!   fight in.
//! * `offscreen_battle_*` draw formation 301 (scene 75, slot 1: two MPs, location code 9) -- the
//!   enemies posed at slot 0 and later frames, the location, the whole battle through the formation's
//!   own `scene.bin` camera -- and the playable characters (Cloud with the Buster Sword).
//!
//! # Camera and placement
//!
//! `scene.bin` battle space is `fmv_ff7::battle_model`'s native space (units: battle units; `y` down,
//! ground at 0; a model faces `-z`). A camera record is a position and a focus point in that space;
//! the camera here looks from the position at the focus with the world's down as the image's down,
//! so (for the records the game uses) `+z` is to the viewer's right. The models are skinned into
//! camera space by [`skin`] exactly as a field's are, only with the battle placement
//! `translate(position) * rotate_y(heading)` instead of the field's.

use super::real;
use super::*;
use crate::avatar::frame::{skin, SkinnedModel};
use crate::avatar::library::AvatarAsset;
use crate::avatar::source::NON_PLAYABLE;
use crate::ui::testkit::write_png;
use fmv_ff7::battle_model::skeleton::BattleModelKind;
use fmv_ff7::battle_model::{BattleCharacter, BattleModel, BattleModels};
use fmv_ff7::model::{mat4_identity, mat4_mul, mat4_translate};
use fmv_ff7::{FieldAnimation, FieldModel};
use fmv_ff7_battle::scene::{SceneBin, FORMATIONS_PER_SCENE};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------------------------
// Where the data is
// ---------------------------------------------------------------------------------------------

/// The owner's battle data, opened read-only.
struct BattleData {
    models: BattleModels,
    scenes: SceneBin,
}

/// `scene.bin` of a game's `data/` directory: the classic place, else the first language directory
/// (`lang-en/battle/scene.bin` in the Steam layout).
fn scene_in_data_dir(data: &Path) -> Option<PathBuf> {
    let classic = data.join("battle").join("scene.bin");
    if classic.is_file() {
        return Some(classic);
    }
    let mut langs: Vec<PathBuf> = std::fs::read_dir(data)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("lang-")))
        .collect();
    langs.sort();
    langs.into_iter().map(|l| l.join("battle").join("scene.bin")).find(|p| p.is_file())
}

/// The `battle.lgp` and `scene.bin` of whichever source the environment names: `FMV_FF7_LIBRARY`
/// (the state directory of the imported library), `FMV_FF7_DATA` (the game's `data/` directory) or
/// `FMV_FF7_FLEVEL` (a `flevel.lgp` of an install's `field/` folder). `None` (with a note) when none
/// is set or the files are not there.
fn open_battle() -> Option<BattleData> {
    use fmv_library::ff7::{resolve_battle_files, resolve_battle_lgp, Ff7Labels, Ff7Source};
    let direct = |path: PathBuf| Ff7Source::Flevel { path, labels: Ff7Labels::default() };
    let (lgp, scene) = if let Some(dir) = std::env::var_os("FMV_FF7_LIBRARY") {
        let source = Ff7Source::Library { state_dir: Some(dir.into()), set: None };
        let lgp = resolve_battle_lgp(&source).expect("the library is read")?;
        (lgp, resolve_battle_files(&source).expect("the library has scene.bin").scene)
    } else if let Some(dir) = std::env::var_os("FMV_FF7_DATA") {
        let dir = PathBuf::from(dir);
        (dir.join("battle").join("battle.lgp"), scene_in_data_dir(&dir)?)
    } else if let Some(path) = std::env::var_os("FMV_FF7_FLEVEL") {
        let path = PathBuf::from(path);
        let source = direct(path.clone());
        let lgp = resolve_battle_lgp(&source).expect("the install is read")?;
        let data = path.parent()?.parent()?.to_path_buf();
        (lgp, scene_in_data_dir(&data)?)
    } else {
        eprintln!("skipped: none of FMV_FF7_LIBRARY, FMV_FF7_DATA, FMV_FF7_FLEVEL is set");
        return None;
    };
    if !lgp.is_file() {
        eprintln!("skipped: no battle.lgp found");
        return None;
    }
    let models = BattleModels::open(&lgp).expect("battle.lgp opens");
    let scenes = SceneBin::open(&scene).expect("scene.bin opens");
    Some(BattleData { models, scenes })
}

fn shot_dir() -> PathBuf {
    match std::env::var_os("FMV_SHOT_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| ".".into())).join(".cache/claude-tmp/fmv-shots/sw-bmodel"),
    }
}

fn save_png(name: &str, w: u32, h: u32, rgba: &[u8]) -> PathBuf {
    let dir = shot_dir();
    std::fs::create_dir_all(&dir).expect("shot dir");
    let file = dir.join(format!("{name}.png"));
    std::fs::write(&file, write_png(w, h, rgba)).expect("write png");
    eprintln!("wrote {}", file.display());
    file
}

// ---------------------------------------------------------------------------------------------
// Mappings against scene.bin and the field archive (no GPU)
// ---------------------------------------------------------------------------------------------

/// What the 256 scenes of `scene.bin` say about the models: every enemy id and location code they
/// use, and where the enemies' animation indices land.
#[derive(Debug, Default)]
struct SceneUse {
    enemy_ids: BTreeSet<u16>,
    enemy_slots: usize,
    location_codes: BTreeSet<u16>,
    no_location: usize,
    /// (real slot, special entry, beyond the model's slots) over every animation index in use.
    refs: (usize, usize, usize),
}

fn scene_use(data: &mut BattleData) -> SceneUse {
    let mut out = SceneUse::default();
    let mut slot_kinds: BTreeMap<u16, Vec<bool>> = BTreeMap::new();
    for scene in data.scenes.scenes() {
        for (k, id) in scene.enemy_ids.iter().enumerate() {
            let Some(id) = *id else { continue };
            out.enemy_ids.insert(id);
            out.enemy_slots += 1;
            let Some(record) = scene.enemies.get(k).and_then(|e| e.as_ref()) else { continue };
            let slots = slot_kinds.entry(id).or_insert_with(|| data.models.load_enemy(id).map(|m| m.slot_ok).unwrap_or_default());
            for (action, &index) in record.animation_indices.iter().enumerate() {
                // 0xFF is an unused action; an index counts only for an action whose attack the record uses
                if index == 0xFF || record.attack_ids.get(action).copied().unwrap_or(0xFFFF) == 0xFFFF {
                    continue;
                }
                match slots.get(usize::from(index)) {
                    Some(true) => out.refs.0 += 1,
                    Some(false) => out.refs.1 += 1,
                    None => out.refs.2 += 1,
                }
            }
        }
        for setup in &scene.setups {
            if setup.location == 0xFFFF {
                out.no_location += 1;
            } else {
                out.location_codes.insert(setup.location);
            }
        }
    }
    out
}

#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_LIBRARY, FMV_FF7_DATA or FMV_FF7_FLEVEL)"]
fn real_battle_ids_and_codes_agree_with_scene_bin() {
    let Some(mut data) = open_battle() else { return };
    assert_eq!(data.scenes.scenes().len(), 256);
    let used = scene_use(&mut data);
    eprintln!(
        "scene.bin: {} enemy slots naming {} distinct enemy ids ({}..={}), {} location codes ({}..={}), {} setups without one; animation references: {} on real slots, {} on special entries, {} beyond the model's slots",
        used.enemy_slots,
        used.enemy_ids.len(),
        used.enemy_ids.first().copied().unwrap_or(0),
        used.enemy_ids.last().copied().unwrap_or(0),
        used.location_codes.len(),
        used.location_codes.first().copied().unwrap_or(0),
        used.location_codes.last().copied().unwrap_or(0),
        used.no_location,
        used.refs.0,
        used.refs.1,
        used.refs.2,
    );
    // every enemy id the scenes use is an enemy model (the id is the model id)
    for id in &used.enemy_ids {
        let prefix = data.models.enemy_prefix(*id).unwrap_or_else(|| panic!("enemy id {id} has no model"));
        assert_eq!(data.models.kind_of(&prefix), Some(BattleModelKind::Enemy));
    }
    assert_eq!((used.enemy_slots, used.enemy_ids.len()), (625, 352));
    assert_eq!(*used.enemy_ids.last().unwrap(), 364);
    // every location code is a location model, and they are 0..=89 but for 82
    assert_eq!(used.location_codes.len(), 89);
    assert!(used.location_codes.iter().all(|c| *c <= 89 && *c != 82));
    for code in &used.location_codes {
        assert!(data.models.location_prefix(*code).is_some(), "location code {code}");
    }
    assert_eq!(used.no_location, 28);
    // the animation indices of the enemy records land on the model's slots (see `battle_model`)
    assert_eq!(used.refs, (2148, 45, 309));
}

/// For each field with random encounters: the battle locations of the formations it fights.
fn field_locations(real: &real::Real, scenes: &SceneBin) -> BTreeMap<String, BTreeSet<u16>> {
    use fmv_adapter_api::{AreaRuntime, GameAdapter};
    let mut out = BTreeMap::new();
    for info in real.adapter.list_areas() {
        let Ok(area) = real.adapter.load_field(&info.id.key) else { continue };
        let Some(table) = area.encounters() else { continue };
        let codes: BTreeSet<u16> = table
            .entries
            .iter()
            .filter_map(|e| scenes.formation(usize::from(e.formation)))
            .map(|f| f.setup().location)
            .collect();
        out.insert(info.id.key.clone(), codes);
    }
    out
}

#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_LIBRARY, FMV_FF7_DATA or FMV_FF7_FLEVEL)"]
fn real_battle_locations_follow_the_fields_that_fight_in_them() {
    let Some(data) = open_battle() else { return };
    let Some(real) = real::open() else { return };
    let by_field = field_locations(&real, &data.scenes);
    let places = by_field.values().flatten().collect::<BTreeSet<_>>();
    eprintln!("{} fields have random encounters, in {} distinct battle locations", by_field.len(), places.len());
    // Fields of one place fight in one location: the three fields of the first mission's Mako
    // reactor (the md1stin battle of formation 301 and the two after it) all in code 9, the ice cave
    // in 50, the crater in 62 -- the codes the loader maps to `op`, `qe` and `qq`.
    let only = |field: &str, code: u16| {
        let codes = by_field.get(field).unwrap_or_else(|| panic!("{field} has random encounters"));
        eprintln!("field {field:8} fights in location code(s) {codes:?}");
        assert_eq!(codes, &BTreeSet::from([code]), "{field}");
    };
    for field in ["md1stin", "md1_1", "md1_2"] {
        only(field, 9);
    }
    only("icedun_1", 50);
    only("icedun_2", 50);
    only("crater_1", 62);
    // and the places differ: the reactor is not the ice cave
    assert_eq!(data.models.location_prefix(9).as_deref(), Some("op"));
    assert_eq!(data.models.location_prefix(50).as_deref(), Some("qe"));
    assert_eq!(data.models.location_prefix(62).as_deref(), Some("qq"));
    // every random-encounter formation of every field has a battle location that exists
    for (field, codes) in &by_field {
        for code in codes {
            assert!(data.models.location_prefix(*code).is_some(), "{field}: location {code}");
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Drawing: cameras, placement, props
// ---------------------------------------------------------------------------------------------

type Mat64 = [f64; 16];

fn sub3(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot3(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross3(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn unit3(a: [f64; 3]) -> [f64; 3] {
    let n = dot3(a, a).sqrt().max(1e-12);
    [a[0] / n, a[1] / n, a[2] / n]
}

/// The world -> camera matrix (column-major, camera space: x right, y down, z forward) of a camera
/// at `position` looking at `focus`, with the world's down as the picture's down. Rows are
/// `right = down x forward`, `down`, `forward`: a rotation (determinant +1), so nothing is
/// mirrored; in battle space this puts `+z` on the viewer's right for the cameras the game uses.
fn look_at(position: [f64; 3], focus: [f64; 3]) -> Mat64 {
    let forward = unit3(sub3(focus, position));
    let world_down = [0.0, 1.0, 0.0];
    let along = dot3(world_down, forward);
    let down = unit3([world_down[0] - forward[0] * along, world_down[1] - forward[1] * along, world_down[2] - forward[2] * along]);
    let right = cross3(down, forward);
    let t = [-dot3(right, position), -dot3(down, position), -dot3(forward, position)];
    [right[0], down[0], forward[0], 0.0, right[1], down[1], forward[1], 0.0, right[2], down[2], forward[2], 0.0, t[0], t[1], t[2], 1.0]
}

/// A battle camera: the matrix and the focal length in pixels of the picture it is drawn in.
#[derive(Clone, Copy)]
struct View {
    camera: Mat64,
    /// Focal length in pixels: a point at camera-space `(x, y, z)` lands at `(w/2 + zoom x / z, h/2 + zoom y / z)`.
    zoom: f32,
    w: u32,
    h: u32,
}

impl View {
    fn new(position: [f64; 3], focus: [f64; 3], zoom: f32, w: u32, h: u32) -> Self {
        Self { camera: look_at(position, focus), zoom, w, h }
    }

    /// Where a battle-space point is in the picture (pixels from the top left) and its camera depth.
    fn project(&self, p: [f32; 3]) -> Option<(f32, f32, f32)> {
        let m = &self.camera;
        let p = p.map(f64::from);
        let c = [
            m[0] * p[0] + m[4] * p[1] + m[8] * p[2] + m[12],
            m[1] * p[0] + m[5] * p[1] + m[9] * p[2] + m[13],
            m[2] * p[0] + m[6] * p[1] + m[10] * p[2] + m[14],
        ];
        (c[2] > 1.0).then(|| {
            (
                (f64::from(self.w) / 2.0 + f64::from(self.zoom) * c[0] / c[2]) as f32,
                (f64::from(self.h) / 2.0 + f64::from(self.zoom) * c[1] / c[2]) as f32,
                c[2] as f32,
            )
        })
    }
}

/// Rotation about the vertical (y) axis by `heading` radians: the model's `-z` forward goes to
/// `(-sin, 0, -cos)`, so 0 faces `-z` (the party's way, towards the enemies) and half a turn faces `+z`.
fn rotate_y(heading: f32) -> [f32; 16] {
    let (s, c) = heading.sin_cos();
    [c, 0.0, -s, 0.0, 0.0, 1.0, 0.0, 0.0, s, 0.0, c, 0.0, 0.0, 0.0, 0.0, 1.0]
}

/// Native model space -> battle space for a model standing at `at` (its soles at `y = 0` there).
fn place(at: [f32; 3], heading: f32) -> [f32; 16] {
    mat4_mul(&mat4_translate(at), &rotate_y(heading))
}

/// A model that can be drawn many times: its merged mesh (uploaded to the GPU once) and what poses it.
struct Prop {
    asset: Arc<AvatarAsset>,
    model: FieldModel,
}

impl Prop {
    /// `clip` is only what the asset's idle pose (its height) is measured in.
    fn new(model: &FieldModel, clip: &FieldAnimation) -> Self {
        let stand = Character { appearance: NON_PLAYABLE, model: model.clone(), idle: clip.clone(), walk: clip.clone(), run: clip.clone() };
        Self { asset: Arc::new(AvatarAsset::new(stand).expect("the battle model merges into a mesh")), model: model.clone() }
    }

    /// The model in `clip` at `frame`, placed by `at`, skinned into the camera of `view`.
    fn skinned(&self, clip: &FieldAnimation, frame: usize, at: &[f32; 16], view: &View) -> SkinnedModel {
        let pose = self.model.pose(clip, frame).expect("the clip fits the model");
        let mut positions = Vec::new();
        assert!(skin(&self.asset, &pose, at, &view.camera, &mut positions), "the model skins");
        SkinnedModel { asset: Arc::clone(&self.asset), positions }
    }
}

/// A character with its body and its weapon 0 as props.
struct Hero {
    model: BattleModel,
    body: Prop,
    weapon: Option<Prop>,
}

impl Hero {
    fn new(model: BattleModel) -> Self {
        let body = Prop::new(&model.model, &model.animations[0]);
        let weapon = match (model.weapons.first(), model.weapon_tracks.first()) {
            (Some(w), Some(track)) => Some(Prop::new(w, track)),
            _ => None,
        };
        Self { model, body, weapon }
    }

    /// The body and the weapon in slot `slot` at `frame`.
    fn draw(&self, slot: usize, frame: usize, at: &[f32; 16], view: &View) -> Vec<SkinnedModel> {
        let mut out = vec![self.body.skinned(&self.model.animations[slot], frame, at, view)];
        if let (Some(weapon), Some(track)) = (&self.weapon, self.model.weapon_tracks.get(slot)) {
            out.push(weapon.skinned(track, frame, at, view));
        }
        out
    }
}

impl Hero {
    /// The bounds (battle space, y down) of the body and weapon 0 in slot `slot` at `frame`, standing at the origin.
    fn bounds(&self, slot: usize, frame: usize) -> ([f32; 3], [f32; 3]) {
        let pose = self.model.model.pose(&self.model.animations[slot], frame).expect("poses");
        let (mut lo, mut hi) = self.model.model.posed_bounds(&pose, &mat4_identity()).expect("has vertices");
        if let (Some(weapon), Some(m)) = (self.model.weapons.first(), self.model.weapon_pose(0, slot, frame)) {
            if let Some((wlo, whi)) = weapon.posed_bounds(&[m], &mat4_identity()) {
                for k in 0..3 {
                    lo[k] = lo[k].min(wlo[k]);
                    hi[k] = hi[k].max(whi[k]);
                }
            }
        }
        (lo, hi)
    }
}

/// A camera in front of a model (a model faces `-z`, so the camera is at `-z` of it) that fits the
/// bounds `lo..hi` (battle space, y down) in a `w` x `h` picture with some room around them.
fn front_view(lo: [f32; 3], hi: [f32; 3], w: u32, h: u32) -> View {
    let zoom = 1400.0f32;
    let by_height = (hi[1] - lo[1]).max(1.0) * 1.25 * zoom / h as f32;
    let by_width = (hi[0] - lo[0]).max(1.0) * 1.25 * zoom / w as f32;
    let distance = f64::from(by_height.max(by_width));
    let centre = [f64::from(lo[0] + hi[0]) / 2.0, f64::from(lo[1] + hi[1]) / 2.0, f64::from(lo[2] + hi[2]) / 2.0];
    View::new([centre[0], centre[1] - 60.0, centre[2] - distance], centre, zoom, w, h)
}

/// How far the models of a battle picture are pulled towards the camera when they are overlays
/// (a face on a head), in battle units (the field's 0.12 model units are about 0.4% of a model).
const OVERLAY_BIAS: f32 = 3.0;

fn frame_of(models: Vec<SkinnedModel>, view: &View) -> AvatarFrame {
    AvatarFrame { models, zoom: view.zoom, overlay_bias: OVERLAY_BIAS, ground: vec![], scenery: None }
}

/// Draw `frame` in a `view.w` x `view.h` window whose pixels are the camera's: the fitted camera
/// has one pixel per view unit, so `zoom` is the focal length in pixels.
fn draw(gpu: &mut Offscreen, frame: &AvatarFrame, view: &View) -> Vec<u8> {
    let half = (view.w as f32 / 2.0 - MARGIN_PX, view.h as f32 / 2.0 - MARGIN_PX);
    let bounds = Bounds { min: Pos::new(-half.0, -half.1, 0.0), max: Pos::new(half.0, half.1, 0.0) };
    let scene = Scene { bounds, triangles: &[], players: &[], fixture_banner: false, backdrop: None };
    gpu.render_field(view.w, view.h, &scene, Some(frame), &DrawList::new())
}

/// Several equal-size pictures side by side in `columns` columns.
fn grid(tiles: &[Vec<u8>], tile_w: u32, tile_h: u32, columns: usize) -> (u32, u32, Vec<u8>) {
    let rows = tiles.len().div_ceil(columns);
    let (w, h) = (tile_w as usize * columns, tile_h as usize * rows);
    let mut out = vec![0u8; w * h * 4];
    for (i, tile) in tiles.iter().enumerate() {
        let (cx, cy) = ((i % columns) * tile_w as usize, (i / columns) * tile_h as usize);
        for y in 0..tile_h as usize {
            let src = &tile[y * tile_w as usize * 4..(y + 1) * tile_w as usize * 4];
            let dst = ((cy + y) * w + cx) * 4;
            out[dst..dst + src.len()].copy_from_slice(src);
        }
    }
    (w as u32, h as u32, out)
}

/// The number of pixels of `img` that differ from `base` by more than a few levels in `rect`
/// (`x0, y0, x1, y1`, clamped to the picture).
fn changed_in(img: &[u8], base: &[u8], w: u32, h: u32, rect: (i32, i32, i32, i32)) -> usize {
    let (x0, y0) = (rect.0.clamp(0, w as i32) as u32, rect.1.clamp(0, h as i32) as u32);
    let (x1, y1) = (rect.2.clamp(0, w as i32) as u32, rect.3.clamp(0, h as i32) as u32);
    let mut n = 0;
    for y in y0..y1 {
        for x in x0..x1 {
            let (a, b) = (pixel(img, w, x, y), pixel(base, w, x, y));
            n += usize::from(a.iter().zip(b).any(|(p, q)| p.abs_diff(q) > 6));
        }
    }
    n
}

// ---------------------------------------------------------------------------------------------
// Formation 301 (scene 75, slot 1): two MPs in the Mako reactor
// ---------------------------------------------------------------------------------------------

/// The formation's enemies as (enemy id, battle position).
fn formation_enemies(scenes: &SceneBin, global: usize) -> Vec<(u16, [f32; 3])> {
    let formation = scenes.formation(global).expect("the formation exists");
    formation.formation().entries.iter().map(|e| (e.enemy_id, e.position.map(f32::from))).collect()
}

const FORMATION: usize = 301;

#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_LIBRARY, FMV_FF7_DATA or FMV_FF7_FLEVEL) and a GPU or software Vulkan/GL driver"]
fn offscreen_battle_formation_301_enemies_posed_at_several_frames() {
    let Some(mut data) = open_battle() else { return };
    let Some(mut gpu) = Offscreen::new() else { return };
    assert_eq!(FORMATION, 75 * FORMATIONS_PER_SCENE + 1);
    let enemies = formation_enemies(&data.scenes, FORMATION);
    eprintln!("formation {FORMATION}: {enemies:?}");
    assert_eq!(enemies.len(), 2, "two enemies");
    assert!(enemies.iter().all(|(id, _)| *id == 16), "both are enemy 16, the MP");
    let mp = data.models.load_enemy(16).expect("the MP loads");
    assert_eq!((mp.prefix.as_str(), mp.model.bones.len(), mp.animations.len()), ("aq", 27, 16));
    let prop = Prop::new(&mp.model, &mp.animations[0]);

    // In front of the enemies, looking at them (they face +z, towards the party): they stand
    // 1 000 units apart, 1 500 and 1 700 away from the party's side.
    let (w, h) = (1280u32, 720u32);
    let view = View::new([0.0, -650.0, 300.0], [0.0, -380.0, -1600.0], 1250.0, w, h);
    let base = draw(&mut gpu, &frame_of(vec![], &view), &view);
    let mut strip = Vec::new();
    for (i, frame) in [0usize, 4, 8, 12].into_iter().enumerate() {
        let models: Vec<SkinnedModel> = enemies
            .iter()
            .enumerate()
            .map(|(k, (_, at))| {
                // the second MP is a few frames behind the first, as two enemies of one formation are
                prop.skinned(&mp.animations[0], frame + 5 * k, &place(*at, std::f32::consts::PI), &view)
            })
            .collect();
        let img = draw(&mut gpu, &frame_of(models, &view), &view);
        if i == 0 {
            save_png("01-enemies-formation301-slot0-frame00", w, h, &img);
        }
        // each MP's silhouette is where the camera puts its feet and head
        for (k, (_, at)) in enemies.iter().enumerate() {
            let feet = view.project(*at).expect("feet in front of the camera");
            let head = view.project([at[0], at[1] - 790.0, at[2]]).expect("head in front of the camera");
            let rect = ((feet.0 - 120.0) as i32, head.1 as i32 - 4, (feet.0 + 120.0) as i32, feet.1 as i32 + 4);
            let n = changed_in(&img, &base, w, h, rect);
            eprintln!("slot 0 frame {}: MP {k} feet at ({:.0}, {:.0}), head at y {:.0}, {n} changed pixels in its box", frame + 5 * k, feet.0, feet.1, head.1);
            assert!(n > 4000, "MP {k} is drawn where the camera puts it ({n} pixels)");
            assert!(head.1 > 20.0 && feet.1 < h as f32 - 20.0, "MP {k} is inside the picture");
        }
        strip.push(img);
    }
    // the four frames side by side in one picture, to compare the poses
    let small: Vec<Vec<u8>> = strip
        .iter()
        .map(|img| {
            // every second pixel of every second row: 640 x 360
            let mut out = Vec::with_capacity((w / 2 * h / 2 * 4) as usize);
            for y in (0..h).step_by(2) {
                for x in (0..w).step_by(2) {
                    let at = ((y * w + x) * 4) as usize;
                    out.extend_from_slice(&img[at..at + 4]);
                }
            }
            out
        })
        .collect();
    let (gw, gh, sheet) = grid(&small, w / 2, h / 2, 2);
    save_png("01-enemies-formation301-slot0-frames-0-4-8-12", gw, gh, &sheet);
    // the frames really differ (the clip moves)
    assert!(strip[0] != strip[1] && strip[1] != strip[2] && strip[2] != strip[3]);

    // The other animation slots of the MP, mid-clip: the hurt, attack and other poses are big
    // motions of the whole skeleton, so a wrong decode or a wrong slot would show here. Seen from
    // the side (the camera at +x: +z, the way the MP faces, is to the right), because several of
    // them carry the MP a thousand units forward (the root's `z`: slot 2 runs to the target, 3 and 4
    // attack there, 5 comes back) and the picture should follow it, not hide it behind the camera.
    let single = View::new([6200.0, -450.0, -1100.0], [0.0, -400.0, -1100.0], 1100.0, 480, 360);
    let at = place([0.0, 0.0, -1600.0], std::f32::consts::PI);
    let mut tiles = Vec::new();
    let mut shown = Vec::new();
    for slot in 0..8usize {
        if !mp.slot_ok[slot] {
            continue;
        }
        let frame = mp.animations[slot].frame_count() / 2;
        let img = draw(&mut gpu, &frame_of(vec![prop.skinned(&mp.animations[slot], frame, &at, &single)], &single), &single);
        tiles.push(img);
        shown.push((slot, frame, mp.animations[slot].frame_count()));
    }
    eprintln!("MP slots shown (slot, frame, frames): {shown:?}");
    let (gw, gh, sheet) = grid(&tiles, 480, 360, 4);
    save_png("01b-enemy-mp-animation-slots-mid-clip", gw, gh, &sheet);
    assert!(tiles.windows(2).all(|p| p[0] != p[1]), "every slot is a different pose");
}

// ---------------------------------------------------------------------------------------------
// The battle location of formation 301
// ---------------------------------------------------------------------------------------------

/// The camera record `index` (0..3) of a formation's `scene.bin` camera placement: a position and
/// a focus point in battle units.
fn scene_view(scenes: &SceneBin, global: usize, index: usize, zoom: f32, w: u32, h: u32) -> View {
    let formation = scenes.formation(global).expect("the formation exists");
    let camera = formation.camera().cameras[index];
    View::new(camera.position.map(f64::from), camera.direction.map(f64::from), zoom, w, h)
}

#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_LIBRARY, FMV_FF7_DATA or FMV_FF7_FLEVEL) and a GPU or software Vulkan/GL driver"]
fn offscreen_battle_location_of_formation_301_with_its_enemies_on_the_floor() {
    let Some(mut data) = open_battle() else { return };
    let Some(mut gpu) = Offscreen::new() else { return };
    let formation = data.scenes.formation(FORMATION).expect("formation 301");
    let code = formation.setup().location;
    assert_eq!(code, 9, "formation 301 fights in battle location 9");
    let enemies = formation_enemies(&data.scenes, FORMATION);
    let arena = data.models.load_location(code).expect("the location loads");
    let mp = data.models.load_enemy(16).expect("the MP loads");
    eprintln!(
        "location {code} = model {}: {} parts, {} triangles, {} textured parts",
        arena.prefix,
        arena.model.parts.len(),
        arena.model.triangle_count(),
        arena.model.parts.iter().filter(|p| p.texture.is_some()).count()
    );
    let location = Prop::new(&arena.model, &arena.animations[0]);
    let mp_prop = Prop::new(&mp.model, &mp.animations[0]);

    let (w, h) = (1280u32, 720u32);
    // Through the formation's own cameras (the game's idle camera and the one its attacks cut to).
    for (name, index, zoom) in [("scene-camera-0", 0usize, 1800.0f32), ("scene-camera-1", 1, 1800.0)] {
        let view = scene_view(&data.scenes, FORMATION, index, zoom, w, h);
        let arena_only = draw(&mut gpu, &frame_of(vec![location.skinned(&arena.animations[0], 0, &mat4_identity(), &view)], &view), &view);
        let mut models = vec![location.skinned(&arena.animations[0], 0, &mat4_identity(), &view)];
        for (k, (_, at)) in enemies.iter().enumerate() {
            models.push(mp_prop.skinned(&mp.animations[0], 3 * k, &place(*at, std::f32::consts::PI), &view));
        }
        let img = draw(&mut gpu, &frame_of(models, &view), &view);
        save_png(&format!("02-location-{code}-{}-formation301-enemies-on-the-floor-{name}", arena.prefix), w, h, &img);
        // Both enemies are inside the picture, and what is under each one's feet in the picture
        // without them is the floor (something other than the empty background).
        for (k, (_, at)) in enemies.iter().enumerate() {
            let feet = view.project(*at).expect("in front of the camera");
            let head = view.project([at[0], at[1] - 790.0, at[2]]).expect("in front of the camera");
            eprintln!("{name}: MP {k} feet at ({:.0}, {:.0}), {:.0} px tall", feet.0, feet.1, feet.1 - head.1);
            assert!(feet.0 > 40.0 && feet.0 < w as f32 - 40.0 && head.1 > 10.0 && feet.1 < h as f32 - 10.0, "MP {k} is inside the camera's picture");
            let under = pixel(&arena_only, w, feet.0 as u32, feet.1 as u32 + 4);
            assert!(!close(under, CLEAR_RGB, 6), "there is floor under MP {k}'s feet: {under:?}");
            let n = changed_in(&img, &arena_only, w, h, ((feet.0 - 90.0) as i32, head.1 as i32, (feet.0 + 90.0) as i32, feet.1 as i32));
            assert!(n > 800, "MP {k} is drawn over the location ({n} px)");
        }
        // the location fills the picture: nearly no pixel is the empty background
        let empty = (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).filter(|&(x, y)| close(pixel(&img, w, x, y), CLEAR_RGB, 3)).count();
        eprintln!("{name}: {empty} empty background pixels of {}", w * h);
        assert!(empty < (w * h) as usize / 10, "the location fills the picture");
    }

    // An overview from above and to the side, inside the dome: the floor, the walls, the sky.
    let view = View::new([9000.0, -7000.0, 9000.0], [0.0, -300.0, -500.0], 1100.0, w, h);
    let mut models = vec![location.skinned(&arena.animations[0], 0, &mat4_identity(), &view)];
    for (k, (_, at)) in enemies.iter().enumerate() {
        models.push(mp_prop.skinned(&mp.animations[0], 3 * k, &place(*at, std::f32::consts::PI), &view));
    }
    let img = draw(&mut gpu, &frame_of(models, &view), &view);
    save_png(&format!("02b-location-{code}-{}-overview", arena.prefix), w, h, &img);

    // Other places the fields' encounter tables point at, each through the camera of a formation
    // that fights there: the reactor (9), the ice cave (50, the `icedun` fields), the crater (62) and
    // the grassland (2, the most used code: 79 setups), so the pictures can be told apart.
    let mut tiles = Vec::new();
    for code in [9u16, 50, 62, 2] {
        let formation = formation_using(&data.scenes, code).expect("a formation fights there");
        let view = scene_view(&data.scenes, formation, 0, 1800.0, 640, 360);
        let arena = data.models.load_location(code).expect("loads");
        let prop = Prop::new(&arena.model, &arena.animations[0]);
        tiles.push(draw(&mut gpu, &frame_of(vec![prop.skinned(&arena.animations[0], 0, &mat4_identity(), &view)], &view), &view));
    }
    let (gw, gh, sheet) = grid(&tiles, 640, 360, 2);
    save_png("02c-locations-9-reactor-50-ice-cave-62-crater-2-grassland", gw, gh, &sheet);
    assert!(tiles.windows(2).all(|p| p[0] != p[1]), "different places look different");
}

/// The first formation (global index) whose battle setup is location `code`.
fn formation_using(scenes: &SceneBin, code: u16) -> Option<usize> {
    (0..scenes.formation_count()).find(|&g| scenes.formation(g).is_some_and(|f| f.setup().location == code))
}

fn close(a: [u8; 3], b: [u8; 3], tol: u8) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= tol)
}

// ---------------------------------------------------------------------------------------------
// Cloud and the other playable characters
// ---------------------------------------------------------------------------------------------

#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_LIBRARY, FMV_FF7_DATA or FMV_FF7_FLEVEL) and a GPU or software Vulkan/GL driver"]
fn offscreen_battle_cloud_with_the_buster_sword() {
    let Some(mut data) = open_battle() else { return };
    let Some(mut gpu) = Offscreen::new() else { return };
    let cloud = data.models.load_character(BattleCharacter::Cloud).expect("Cloud loads");
    eprintln!(
        "Cloud {}: {} bones, {} parts, {} slots ({} real), {} weapons, {} weapon tracks",
        cloud.prefix,
        cloud.model.bones.len(),
        cloud.model.parts.len(),
        cloud.animations.len(),
        cloud.slot_ok.iter().filter(|ok| **ok).count(),
        cloud.weapons.len(),
        cloud.weapon_tracks.len()
    );
    let real_slots: Vec<(usize, usize)> =
        cloud.slot_ok.iter().enumerate().filter(|(_, ok)| **ok).map(|(i, _)| (i, cloud.animations[i].frame_count())).collect();
    eprintln!("Cloud's real slots (slot, frames): {real_slots:?}");
    let hero = Hero::new(cloud);

    // From the front (Cloud faces -z, so the camera is at -z), idle clip frame 0 and later frames.
    let (w, h) = (960u32, 720u32);
    let view = View::new([0.0, -520.0, -2900.0], [0.0, -440.0, 0.0], 1600.0, w, h);
    let at = place([0.0, 0.0, 0.0], 0.0);
    let base = draw(&mut gpu, &frame_of(vec![], &view), &view);
    let frames = hero.model.animations[0].frame_count();
    eprintln!("idle clip: {frames} frames");
    let img = draw(&mut gpu, &frame_of(hero.draw(0, 0, &at, &view), &view), &view);
    save_png("03-cloud-slot0-frame00-with-buster-sword", w, h, &img);
    let feet = view.project([0.0, 0.0, 0.0]).expect("feet");
    let head = view.project([0.0, -860.0, 0.0]).expect("head");
    let n = changed_in(&img, &base, w, h, ((feet.0 - 260.0) as i32, head.1 as i32 - 30, (feet.0 + 260.0) as i32, feet.1 as i32 + 10));
    eprintln!("Cloud: feet at ({:.0}, {:.0}), head at y {:.0}, {n} changed pixels", feet.0, feet.1, head.1);
    assert!(n > 20_000, "Cloud is drawn ({n} px)");

    // The idle clip over time, framed tightly: four frames side by side.
    let (lo, hi) = hero.bounds(0, 0);
    eprintln!("Cloud's idle pose spans x {:.0}..{:.0}, y {:.0}..{:.0}, z {:.0}..{:.0} (battle units, y down)", lo[0], hi[0], lo[1], hi[1], lo[2], hi[2]);
    let tight = front_view(lo, hi, 480, 640);
    let tiles: Vec<Vec<u8>> =
        [0usize, 5, 10, 15].iter().map(|&frame| draw(&mut gpu, &frame_of(hero.draw(0, frame, &at, &tight), &tight), &tight)).collect();
    let (gw, gh, sheet) = grid(&tiles, 480, 640, 4);
    save_png("03b-cloud-slot0-frames-0-5-10-15", gw, gh, &sheet);
    assert!(tiles[0] != tiles[1] && tiles[1] != tiles[2], "the idle clip moves");

    // The weapon track belongs to its slot: the sword is in his hands mid-clip in other slots too
    // (and he moves a long way in some of them, so the frame is wide).
    let wide = front_view([-800.0, -1000.0, -300.0], [800.0, 0.0, 300.0], 480, 400);
    let mut tiles = Vec::new();
    let mut shown = Vec::new();
    for (slot, frames) in real_slots.iter().copied().filter(|(slot, frames)| *slot > 0 && *frames >= 6).take(8) {
        let frame = frames / 2;
        tiles.push(draw(&mut gpu, &frame_of(hero.draw(slot, frame, &at, &wide), &wide), &wide));
        shown.push((slot, frame, frames));
    }
    eprintln!("Cloud slots shown with the sword, mid-clip (slot, frame, frames): {shown:?}");
    let (gw, gh, sheet) = grid(&tiles, 480, 400, 4);
    save_png("03c-cloud-other-slots-mid-clip-with-sword", gw, gh, &sheet);
}

#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_LIBRARY, FMV_FF7_DATA or FMV_FF7_FLEVEL) and a GPU or software Vulkan/GL driver"]
fn offscreen_battle_all_playable_characters_in_their_idle_pose() {
    let Some(mut data) = open_battle() else { return };
    let Some(mut gpu) = Offscreen::new() else { return };
    let (w, h) = (360u32, 480u32);
    let at = place([0.0, 0.0, 0.0], 0.0);
    let mut tiles = Vec::new();
    for which in BattleCharacter::ALL {
        let model = data.models.load_character(which).expect("the character loads");
        assert_eq!(model.kind, BattleModelKind::Character);
        let hero = Hero::new(model);
        // each one framed by its own idle pose (Cait Sith's moogle and Sephiroth are bigger than Cloud)
        let (lo, hi) = hero.bounds(0, 0);
        let view = front_view(lo, hi, w, h);
        let base = draw(&mut gpu, &frame_of(vec![], &view), &view);
        let img = draw(&mut gpu, &frame_of(hero.draw(0, 0, &at, &view), &view), &view);
        let n = changed_in(&img, &base, w, h, (0, 0, w as i32, h as i32));
        eprintln!("{:20} {}: {n} pixels drawn", which.display_name(), which.prefix());
        assert!(n > 5_000, "{which:?} is drawn");
        tiles.push(img);
    }
    let (gw, gh, sheet) = grid(&tiles, w, h, 5);
    save_png("04-characters-cloud-barret-tifa-aerith-red-yuffie-cait-vincent-cid-sephiroth", gw, gh, &sheet);
}

// ---------------------------------------------------------------------------------------------
// The whole battle
// ---------------------------------------------------------------------------------------------

#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_LIBRARY, FMV_FF7_DATA or FMV_FF7_FLEVEL) and a GPU or software Vulkan/GL driver"]
fn offscreen_battle_formation_301_whole_scene_through_the_formations_camera() {
    let Some(mut data) = open_battle() else { return };
    let Some(mut gpu) = Offscreen::new() else { return };
    let enemies = formation_enemies(&data.scenes, FORMATION);
    let code = data.scenes.formation(FORMATION).expect("formation").setup().location;
    let arena = data.models.load_location(code).expect("location");
    let mp = data.models.load_enemy(16).expect("MP");
    let location = Prop::new(&arena.model, &arena.animations[0]);
    let mp_prop = Prop::new(&mp.model, &mp.animations[0]);
    let party: Vec<(Hero, [f32; 3])> = [
        (BattleCharacter::Cloud, [0.0, 0.0, 1500.0]),
        (BattleCharacter::Barret, [600.0, 0.0, 1100.0]),
        (BattleCharacter::Tifa, [-600.0, 0.0, 1900.0]),
    ]
    .into_iter()
    .map(|(c, at)| (Hero::new(data.models.load_character(c).expect("character")), at))
    .collect();

    let (w, h) = (1280u32, 720u32);
    for (name, index, zoom) in [("camera-0", 0usize, 1900.0f32), ("camera-1", 1, 1900.0)] {
        let view = scene_view(&data.scenes, FORMATION, index, zoom, w, h);
        let mut models = vec![location.skinned(&arena.animations[0], 0, &mat4_identity(), &view)];
        for (k, (_, at)) in enemies.iter().enumerate() {
            models.push(mp_prop.skinned(&mp.animations[0], 4 * k, &place(*at, std::f32::consts::PI), &view));
        }
        for (hero, at) in &party {
            models.extend(hero.draw(0, 2, &place(*at, 0.0), &view));
        }
        let img = draw(&mut gpu, &frame_of(models, &view), &view);
        save_png(&format!("05-battle-formation301-party-vs-mps-{name}"), w, h, &img);
        // the party is on the right of the picture and the enemies on the left (+z is to the right)
        let x_of = |at: [f32; 3]| view.project(at).map_or(f32::NAN, |p| p.0);
        let enemy_x: Vec<f32> = enemies.iter().map(|(_, at)| x_of(*at)).collect();
        let party_x: Vec<f32> = party.iter().map(|(_, at)| x_of(*at)).collect();
        eprintln!("{name}: enemies at x {enemy_x:?}, party at x {party_x:?}");
        if index == 0 {
            assert!(enemy_x.iter().all(|e| party_x.iter().all(|p| e < p)), "the enemies are left of the party");
        }
    }
}

