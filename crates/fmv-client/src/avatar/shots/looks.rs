//! The looks of the field bodies and of the characters around them, measured on the owner's own `char.lgp` (read-only;
//! numbers only, no pixel of the game is printed or written by the survey).
//!
//! Run with `FMV_FF7_DATA` (the game's `data/` directory) or `FMV_FF7_LIBRARY`:
//!
//! ```text
//! cargo test -p fmv-client -- --ignored real_looks_survey --test-threads=1 --nocapture
//! ```

use super::real::open;
use crate::avatar::library::AvatarAsset;
use crate::avatar::source::{CharacterLoader, LgpLoader};
use fmv_adapter_api::AreaRuntime;
use fmv_core::AnimState;
use fmv_ff7::model::{mat4_identity, transform_point};
use fmv_ff7::playable::{Character, PlayableModel, PLACEHOLDER_BODIES, PLAYABLE_FIELD_MODELS};

/// Bounds of points.
#[derive(Clone, Copy, Debug)]
pub(super) struct Box3 {
    pub lo: [f32; 3],
    pub hi: [f32; 3],
}

impl Box3 {
    fn of(points: impl IntoIterator<Item = [f32; 3]>) -> Option<Self> {
        let mut out: Option<Self> = None;
        for p in points {
            out = Some(match out {
                None => Self { lo: p, hi: p },
                Some(b) => Self { lo: [b.lo[0].min(p[0]), b.lo[1].min(p[1]), b.lo[2].min(p[2])], hi: [b.hi[0].max(p[0]), b.hi[1].max(p[1]), b.hi[2].max(p[2])] },
            });
        }
        out
    }

    pub fn size(&self) -> [f32; 3] {
        [self.hi[0] - self.lo[0], self.hi[1] - self.lo[1], self.hi[2] - self.lo[2]]
    }
}

/// The idle pose's bounds of the whole model, in model units (y points down in the file's own axes).
pub(super) fn model_box(character: &Character) -> Option<Box3> {
    let pose = character.pose(AnimState::Idle, 0)?;
    let (lo, hi) = character.model.posed_bounds(&pose, &mat4_identity())?;
    Some(Box3 { lo, hi })
}

/// What a textured part's face looks like: how much of its texture is drawn at all, and which half the dark texels (eyes,
/// brows, mouth) are in. Aggregates only.
#[derive(Debug)]
pub(super) struct TextureShare {
    pub w: u32,
    pub h: u32,
    pub opaque: f32,
    pub opaque_left: f32,
    pub opaque_right: f32,
    pub dark_left: u32,
    pub dark_right: u32,
}

pub(super) fn texture_share(img: &fmv_ff7::RgbaImage) -> TextureShare {
    let (w, h) = (img.width, img.height);
    let (mut opaque, mut ol, mut or, mut dl, mut dr) = (0u32, 0u32, 0u32, 0u32, 0u32);
    for y in 0..h {
        for x in 0..w {
            let at = ((y * w + x) * 4) as usize;
            let Some(px) = img.rgba.get(at..at + 4) else { continue };
            if px[3] < 128 {
                continue;
            }
            opaque += 1;
            let left = x < w / 2;
            if left {
                ol += 1;
            } else {
                or += 1;
            }
            if u32::from(px[0]) + u32::from(px[1]) + u32::from(px[2]) < 230 {
                if left {
                    dl += 1;
                } else {
                    dr += 1;
                }
            }
        }
    }
    let total = (w * h).max(1) as f32;
    TextureShare { w, h, opaque: opaque as f32 / total, opaque_left: ol as f32 / (total / 2.0), opaque_right: or as f32 / (total / 2.0), dark_left: dl, dark_right: dr }
}

fn describe(label: &str, asset: &AvatarAsset, cloud_height: f32) {
    let character = &asset.character;
    let model = &character.model;
    let bounds = model_box(character);
    let tris: usize = model.parts.iter().map(|p| p.triangle_count()).sum();
    let (w, d) = bounds.map_or((0.0, 0.0), |b| (b.size()[0], b.size()[2]));
    eprintln!(
        "{label:22} hrc {:5} bones {:2} parts {:3} tris {:4} height {:5.1} ({:3.0}% of Cloud) width {w:5.1} depth {d:5.1}",
        model.hrc_name,
        model.bones.len(),
        model.parts.len(),
        tris,
        asset.height_units,
        100.0 * asset.height_units / cloud_height
    );
    let Some(pose) = character.pose(AnimState::Idle, 0) else { return };
    for (i, part) in model.parts.iter().enumerate() {
        let Some(tex) = &part.texture else { continue };
        let Some(matrix) = pose.get(part.bone_index) else { continue };
        let posed = |p: &[f32; 3]| transform_point(matrix, *p);
        let decal = Box3::of(part.positions.iter().map(posed));
        // The head: every part on the same bone, whatever it is.
        let head = Box3::of(model.parts.iter().filter(|q| q.bone_index == part.bone_index).flat_map(|q| q.positions.iter().map(posed)));
        let share = texture_share(tex);
        let uv = part.uvs.iter().fold(([f32::MAX; 2], [f32::MIN; 2]), |(lo, hi), uv| ([lo[0].min(uv[0]), lo[1].min(uv[1])], [hi[0].max(uv[0]), hi[1].max(uv[1])]));
        if let (Some(decal), Some(head)) = (decal, head) {
            eprintln!(
                "    textured part {i} (bone {} {:?}, {} tris, {:?} {}x{}): decal x {:.1}..{:.1} y {:.1}..{:.1} z {:.1}..{:.1}; head x {:.1}..{:.1}; texture {:.0}% drawn (left half {:.0}%, right half {:.0}%), dark texels left {} right {}; uv {:.2}..{:.2} x {:.2}..{:.2}",
                part.bone_index,
                model.bones.get(part.bone_index).map(|b| b.name.as_str()).unwrap_or("?"),
                part.triangle_count(),
                part.blend,
                share.w,
                share.h,
                decal.lo[0],
                decal.hi[0],
                decal.lo[1],
                decal.hi[1],
                decal.lo[2],
                decal.hi[2],
                head.lo[0],
                head.hi[0],
                100.0 * share.opaque,
                100.0 * share.opaque_left,
                100.0 * share.opaque_right,
                share.dark_left,
                share.dark_right,
                uv.0[0],
                uv.1[0],
                uv.0[1],
                uv.1[1],
            );
        }
    }
}

/// The face decals of one model, vertex by vertex (positions in the idle pose, texture coordinates), and the texture
/// as a picture (scaled up, on a grey chequer so that transparent texels show) in `$FMV_SHOT_DIR`.
#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_LIBRARY or FMV_FF7_DATA)"]
fn real_looks_face_decals_vertex_by_vertex() {
    let Some(real) = open() else { return };
    let mut loader = LgpLoader::new(&real.char_lgp);
    let models: Vec<PlayableModel> = PLAYABLE_FIELD_MODELS.iter().take(1).copied().chain(PLACEHOLDER_BODIES.iter().map(|b| b.model)).collect();
    for m in models {
        let character = loader.load(&m).expect("loads");
        let Some(pose) = character.pose(AnimState::Idle, 0) else { continue };
        eprintln!("{}:", m.appearance_ref);
        for (i, part) in character.model.parts.iter().enumerate() {
            let Some(tex) = &part.texture else { continue };
            let Some(matrix) = pose.get(part.bone_index) else { continue };
            eprintln!("  part {i}: {:?} {}x{} blend {:?}", part.texture_name, tex.width, tex.height, part.blend);
            for (v, (p, uv)) in part.positions.iter().zip(&part.uvs).enumerate() {
                let q = transform_point(matrix, *p);
                eprintln!("    v{v}: pos ({:6.2}, {:6.2}, {:6.2})  uv ({:5.2}, {:5.2})  colour {:?}", q[0], q[1], q[2], uv[0], uv[1], part.colors.get(v));
            }
            eprintln!("    triangles {:?}", part.indices.chunks(3).collect::<Vec<_>>());
            // The texture as a picture: 12x, on a chequer.
            let zoom = 12u32;
            let (w, h) = (tex.width * zoom, tex.height * zoom);
            let mut px = vec![255u8; (w * h * 4) as usize];
            for y in 0..h {
                for x in 0..w {
                    let src = (((y / zoom) * tex.width + x / zoom) * 4) as usize;
                    let Some(t) = tex.rgba.get(src..src + 4) else { continue };
                    let chequer = if ((x / 24) + (y / 24)) % 2 == 0 { 150u8 } else { 110 };
                    let a = u32::from(t[3]);
                    let blend = |c: u8, bg: u8| ((u32::from(c) * a + u32::from(bg) * (255 - a)) / 255) as u8;
                    let at = ((y * w + x) * 4) as usize;
                    px[at] = blend(t[0], chequer);
                    px[at + 1] = blend(t[1], chequer);
                    px[at + 2] = blend(t[2], chequer);
                }
            }
            let name = format!("face_texture_{}_part{i}", m.appearance_ref.replace('/', "_"));
            super::real::save_png(&name, w, h, &px);
        }
    }
}

/// The mesh merge takes the whole-number shift off a decal's texture coordinates (`mesh::whole_cell_offsets`), and battle
/// draws every one of its models through the same merge. The battle loader already brings a model's coordinates into
/// `0..1` itself (`fmv_ff7::battle_model::tiling`: the game repeats them, this window's sampler clamps), so the merge must
/// find nothing to take off in any of them. Every character, weapon, enemy and location of the owned `battle.lgp`, read-only:
/// the merged coordinates are exactly the loader's, bit for bit.
#[test]
#[ignore = "needs the owner's real FF7 data: FMV_FF7_DATA with battle/battle.lgp"]
fn real_battle_models_are_not_touched_by_the_merges_unwrap() {
    use crate::avatar::mesh::MergedMesh;
    use fmv_ff7::battle_model::skeleton::BattleModelKind;
    use fmv_ff7::battle_model::{BattleCharacter, BattleModels};
    let Some(data) = std::env::var_os("FMV_FF7_DATA") else {
        eprintln!("skipped: FMV_FF7_DATA is not set");
        return;
    };
    let path = std::path::PathBuf::from(data).join("battle").join("battle.lgp");
    let Ok(mut models) = BattleModels::open(&path) else {
        eprintln!("skipped: no battle/battle.lgp under FMV_FF7_DATA");
        return;
    };
    let (mut checked, mut textured, mut changed) = (0usize, 0usize, 0usize);
    let mut check = |model: &fmv_ff7::FieldModel| {
        let Ok(mesh) = MergedMesh::build(model) else { return };
        let raw: Vec<[f32; 2]> = model.parts.iter().flat_map(|p| (0..p.positions.len()).map(move |i| p.uvs.get(i).copied().unwrap_or([0.0; 2]))).collect();
        checked += 1;
        textured += model.parts.iter().filter(|p| p.texture.is_some()).count();
        if mesh.uvs != raw {
            changed += 1;
        }
    };
    for which in BattleCharacter::ALL {
        if let Ok(m) = models.load_character(which) {
            check(&m.model);
            m.weapons.iter().for_each(&mut check);
        }
    }
    for id in models.ids_of_kind(BattleModelKind::Enemy) {
        if let Ok(m) = models.load_enemy(u16::try_from(id).unwrap_or(u16::MAX)) {
            check(&m.model);
            m.weapons.iter().for_each(&mut check);
        }
    }
    for code in 0..models.location_count() {
        if let Ok(m) = models.load_location(u16::try_from(code).unwrap_or(u16::MAX)) {
            check(&m.model);
        }
    }
    eprintln!("{checked} battle models and weapons, {textured} textured parts: the merge changed the texture coordinates of {changed} of them");
    assert!(checked > 100, "the survey saw {checked} models");
    assert_eq!(changed, 0, "a battle model's coordinates are the loader's, untouched by the merge");
}

#[test]
#[ignore = "needs the owner's real FF7 data (FMV_FF7_LIBRARY or FMV_FF7_DATA)"]
fn real_looks_survey_heights_parts_and_faces() {
    let Some(real) = open() else { return };
    let mut loader = LgpLoader::new(&real.char_lgp);
    let asset = |loader: &mut LgpLoader, m: &PlayableModel| AvatarAsset::new(loader.load(m).expect("loads")).expect("merges");
    let cloud = asset(&mut loader, &PLAYABLE_FIELD_MODELS[0]);
    let cloud_height = cloud.height_units;
    eprintln!("CANON CAST (field models, idle pose, model units; y of the file points down):");
    for m in PLAYABLE_FIELD_MODELS {
        let a = asset(&mut loader, m);
        describe(m.appearance_ref, &a, cloud_height);
    }
    eprintln!("PLACEHOLDER BODIES:");
    for b in PLACEHOLDER_BODIES {
        let a = asset(&mut loader, &b.model);
        describe(b.model.appearance_ref, &a, cloud_height);
    }
    // The characters of the owner's screenshot (`elevtr1`): the NPC `av_j` and its neighbours.
    let key = std::env::var("FMV_FF7_AREA").unwrap_or_else(|_| "elevtr1".to_string());
    let area = real.adapter.load_field(&key).expect("field");
    eprintln!("NPCs OF {key}: field scale {}", area.field_scale());
    for n in area.npcs() {
        let idle = n.appearance.idle_animation.clone().unwrap_or_default();
        match loader.load_npc(&n.appearance.model, &idle) {
            Ok(character) => match AvatarAsset::new(character) {
                Ok(a) => describe(&format!("npc {} ({})", n.entity_name, n.appearance.model), &a, cloud_height),
                Err(e) => eprintln!("npc {}: {e}", n.entity_name),
            },
            Err(e) => eprintln!("npc {}: {e}", n.entity_name),
        }
    }
}
