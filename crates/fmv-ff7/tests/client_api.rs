//! Compile-time pin of the items `fmv-client` (`backdrop.rs`) and the server consume. If one of
//! these signatures or public fields changes, this file stops compiling instead of the client.
//!
//! No game data is involved; nothing here runs a decoder.

use fmv_adapter_api::AdapterError;
use fmv_core::AnimState;
use fmv_ff7::background::{decode_background, decode_background_layers, BackgroundImage, DepthLayers, Projection, ScreenPoint, ScreenRect};
use fmv_ff7::field::{Camera, FieldError, FieldFile};
use fmv_ff7::lgp::LgpArchive;
use fmv_ff7::model::{default_model_scale, load_field_animation, load_field_model, model_to_field, ModelError};
use fmv_ff7::playable::{load_playable, playable_field_models, resolve_appearance, Character, PlayableModel};
use fmv_ff7::{Ff7Adapter, Ff7FieldArea, FieldAnimation, FieldModel, MeshPart, ModelBone, RgbaImage};
use std::io::{Read, Seek};

type Pose = Option<Vec<[f32; 16]>>;

#[test]
fn client_facing_signatures_are_stable() {
    // fmv_ff7::background
    let _: fn(&FieldFile) -> Result<BackgroundImage, FieldError> = decode_background;
    let _: fn(&Camera) -> Projection = Projection::from_camera;
    let _: fn(&Projection, [f32; 3]) -> Option<ScreenPoint> = Projection::project;
    let _: fn(&BackgroundImage) -> ScreenRect = BackgroundImage::bounds;
    let _: fn(&BackgroundImage) -> Vec<u8> = BackgroundImage::flatten_on_black;
    // fmv_ff7 area API
    let _: fn(&Ff7FieldArea) -> Result<Vec<Camera>, FieldError> = Ff7FieldArea::cameras;
    let _: for<'a> fn(&'a Ff7FieldArea) -> &'a FieldFile = Ff7FieldArea::field_file;
    let _: fn(&Ff7Adapter, &str) -> Result<Ff7FieldArea, AdapterError> = Ff7Adapter::load_field;

    // public fields the client reads
    let rect = ScreenRect { min_x: -1, min_y: -2, max_x: 3, max_y: 4 };
    let ScreenRect { min_x, min_y, max_x, max_y } = rect;
    assert_eq!((min_x, min_y, max_x, max_y), (-1, -2, 3, 4));
    let point = ScreenPoint { x: 1.0, y: 2.0, depth: 3.0 };
    let (x, y): (f32, f32) = (point.x, point.y);
    assert_eq!((x, y), (1.0, 2.0));
    let read_image = |image: &BackgroundImage| -> (u32, u32, usize) { (image.width, image.height, image.rgba.len()) };
    let _ = read_image;
}

/// The character-model API the client and the server (appearance ids) are meant to use.
#[test]
fn character_model_signatures_are_stable() {
    // depth layers and speeds
    let _: fn(&FieldFile) -> Result<DepthLayers, FieldError> = decode_background_layers;
    let _: fn(&Ff7FieldArea) -> f32 = Ff7FieldArea::walk_speed;
    let _: fn(&Ff7FieldArea) -> f32 = Ff7FieldArea::run_speed;
    let read_layers = |l: &DepthLayers| -> (u32, u32) { (l.back.width, l.front.height) };
    let _ = read_layers;
    // appearance ids
    let _: fn(&str) -> Option<PlayableModel> = resolve_appearance;
    let _: fn() -> Vec<PlayableModel> = playable_field_models;
    let read_playable = |m: &PlayableModel| -> (&'static str, &'static str, &'static str, &'static str, &'static str, &'static str) {
        (m.appearance_ref, m.display_name, m.hrc, m.idle_anim, m.walk_anim, m.run_anim)
    };
    let _ = read_playable;
    let _: fn(&PlayableModel, AnimState) -> &'static str = PlayableModel::animation_for;
    // loading, posing, placing (generic over any Read + Seek archive reader)
    fn loaders<R: Read + Seek>() {
        let _: fn(&mut LgpArchive<R>, &PlayableModel) -> Result<Character, ModelError> = load_playable::<R>;
        let _: fn(&mut LgpArchive<R>, &str) -> Result<FieldModel, ModelError> = load_field_model::<R>;
        let _: fn(&mut LgpArchive<R>, &str) -> Result<FieldAnimation, ModelError> = load_field_animation::<R>;
    }
    loaders::<std::fs::File>();
    let _: fn(&Character, AnimState, usize) -> Pose = Character::pose;
    let _: for<'a> fn(&'a Character, AnimState) -> &'a FieldAnimation = Character::animation;
    let _: fn(&FieldModel, &FieldAnimation, usize) -> Pose = FieldModel::pose;
    let _: fn(&FieldAnimation, &[ModelBone], usize) -> Pose = FieldAnimation::pose;
    let _: fn(&FieldAnimation, &[ModelBone], f32) -> Pose = FieldAnimation::pose_at;
    let _: fn([f32; 3], f32, f32) -> [f32; 16] = model_to_field;
    let _: fn(u16) -> f32 = default_model_scale;
    // public fields a renderer reads
    let read_part = |p: &MeshPart| {
        let texture: Option<&RgbaImage> = p.texture.as_deref();
        (p.bone_index, p.positions.len(), p.normals.len(), p.uvs.len(), p.colors.len(), p.indices.len(), texture.map(|t| (t.width, t.height, t.rgba.len())))
    };
    let _ = read_part;
    let read_model = |m: &FieldModel| (m.bones.len(), m.parts.len(), m.bones.first().map(|b: &ModelBone| (b.parent, b.length)));
    let _ = read_model;
}
