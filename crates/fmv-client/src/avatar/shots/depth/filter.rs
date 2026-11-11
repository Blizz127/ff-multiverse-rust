//! Field-background filtering through the real GPU paths, without game data. These checks keep
//! texture choice separate from depth, atlas lifetime, model textures and the native UI.

use super::*;
use crate::battle::scene::BattleFrame;
use crate::graphics::BackgroundFilter;

fn filtered_field(
    offscreen: &mut Offscreen, filter: BackgroundFilter, scene: &Scene<'_>, frame: &AvatarFrame,
    overlay: &DrawList,
) -> Vec<u8> {
    offscreen.render_with(W, H, |gpu, target, srgb| {
        gpu.set_background_filter(filter);
        gpu.draw_field(target, (W, H), scene, Some(frame), srgb);
        gpu.draw_overlay(target, &overlay.vertices(W, H, srgb));
    })
}

#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_background_filter_changes_the_cached_atlas_and_restores_pixels_without_uploading() {
    let mut gpu = Offscreen::new().expect("GPU or software adapter");
    let (tiles, picture, canvas) = gradient_picture();
    let bounds = Bounds { min: Pos::new(-32.0, -24.0, 0.0), max: Pos::new(32.0, 24.0, 0.0) };
    let scene = Scene { bounds, triangles: &[], players: &[], fixture_banner: false, backdrop: Some(&picture) };
    let mut frame = off_screen();
    frame.scenery = Some(scenery_of(&tiles, canvas));
    let overlay = DrawList::new();
    let original = gpu.render_field(W, H, &scene, Some(&frame), &overlay);
    let cached = gpu.scenery_state();
    assert_eq!(cached.0, 1, "one atlas upload for the field");
    assert_eq!(cached.1, frame.scenery.as_ref().map(|s| s.id));
    assert_eq!(original, filtered_field(&mut gpu, BackgroundFilter::Linear, &scene, &frame, &overlay),
        "Linear preserves the existing default");
    let nearest = filtered_field(&mut gpu, BackgroundFilter::Nearest, &scene, &frame, &overlay);
    let changed = original.chunks_exact(4).zip(nearest.chunks_exact(4)).filter(|(a, b)| a != b).count();
    assert!(changed > 1_000, "the already cached atlas changes visibly: {changed} pixels");
    assert_eq!(gpu.scenery_state(), cached, "changing the sampler keeps the atlas and draw batches");
    let restored = filtered_field(&mut gpu, BackgroundFilter::Linear, &scene, &frame, &overlay);
    assert_eq!(original, restored, "Linear -> Nearest -> Linear restores every pixel");
    assert_eq!(gpu.scenery_state(), cached, "restoring the sampler does not upload the atlas again");

    // A new field first uploaded while Nearest is chosen must use it too. Identical source pixels
    // with a different scenery ID distinguish this from only rebinding the old atlas.
    let _ = filtered_field(&mut gpu, BackgroundFilter::Nearest, &scene, &frame, &overlay);
    frame.scenery = Some(scenery_of(&tiles, canvas));
    assert_ne!(frame.scenery.as_ref().map(|s| s.id), cached.1);
    let next_field = filtered_field(&mut gpu, BackgroundFilter::Nearest, &scene, &frame, &overlay);
    assert_eq!(nearest, next_field, "new fields use the selected filter on their first frame");
    assert_eq!(gpu.scenery_state().0, cached.0 + 1, "only the new field uploads an atlas");
    assert_eq!(original, filtered_field(&mut gpu, BackgroundFilter::Linear, &scene, &frame, &overlay));
    assert_eq!(gpu.scenery_state().0, cached.0 + 1);
    save("background-filter-tiles-linear", W, H, &original);
    save("background-filter-tiles-nearest", W, H, &nearest);
}

#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_background_filters_preserve_near_far_holes_and_tile_seams() {
    let mut gpu = Offscreen::new().expect("GPU or software adapter");
    let mut frame = built(&[facing_camera(1, "ff7/cloud", Pos::new(0.0, 0.0, 0.0))], false);
    let f = figure(&frame);
    let probe = clear_probe(&mut gpu, &frame, &f);
    let overlay = DrawList::new();
    for filter in [BackgroundFilter::Linear, BackgroundFilter::Nearest] {
        for (depth, want) in [(f.near - 1.0, GREEN), (f.far + 1.0, RED)] {
            frame.scenery = Some(scenery_of(&[tile_over(f.centre, 2, depth, GREEN, None)], canvas()));
            let image = filtered_field(&mut gpu, filter, &scene(), &frame, &overlay);
            assert!(close(pixel(&image, W, f.pixel.0, f.pixel.1), want, 2),
                "{filter:?}: the depth test, not the filter, decides which surface is visible");
            assert!(close(pixel(&image, W, probe.0, probe.1), GREEN, 2), "{filter:?}: tile beside the model");
        }
        let mut hole = tile_over(f.centre, 2, f.near - 100.0, GREEN, None);
        for y in 8..24 { for x in 8..24 {
            hole.rgba[(y * 32 + x) * 4..(y * 32 + x) * 4 + 4].copy_from_slice(&[255, 0, 255, 0]);
        } }
        frame.scenery = Some(scenery_of(&[hole], canvas()));
        let image = filtered_field(&mut gpu, filter, &scene(), &frame, &overlay);
        assert!(close(pixel(&image, W, f.pixel.0, f.pixel.1), RED, 2), "{filter:?}: the hole reveals the model");
        assert!(close(pixel(&image, W, probe.0, probe.1), GREEN, 2), "{filter:?}: the tile remains opaque around it");
        let (left, top) = field_to_pixel(f.centre.0 - 14.0, f.centre.1 - 14.0);
        let (right, bottom) = field_to_pixel(f.centre.0 + 14.0, f.centre.1 + 14.0);
        for y in top..bottom { for x in left..right {
            assert!(pixel(&image, W, x, y)[2] <= 2, "{filter:?}: transparent magenta cannot fringe at {x},{y}");
        } }
        let suffix = if filter == BackgroundFilter::Linear { "linear" } else { "nearest" };
        save(&format!("background-filter-hole-{suffix}"), W, H, &image);

        // Adjacent contrasting tiles at a non-integral magnification: their atlas neighbours may
        // never bleed across the shared edge, and the rasterizer must leave no black seam.
        let mut adjacent = off_screen();
        adjacent.scenery = Some(scenery_of(&[
            solid(2, (-32, -16), 32, 500.0, RED, None),
            solid(2, (0, -16), 32, 600.0, [0, 0, 255], None),
        ], canvas()));
        let image = filtered_field(&mut gpu, filter, &scene(), &adjacent, &overlay);
        let (left, top) = field_to_pixel(-31.0, -15.0);
        let (right, bottom) = field_to_pixel(31.0, 15.0);
        let (mut red, mut blue) = (0, 0);
        for y in top..bottom { for x in left..right {
            match pixel(&image, W, x, y) {
                [255, 0, 0] => red += 1,
                [0, 0, 255] => blue += 1,
                other => panic!("{filter:?}: seam or atlas bleed at {x},{y}: {other:?}"),
            }
        } }
        assert!(red > 100 && blue > 100, "both tiles must be on screen");
    }
}

/// An opaque black/white texture makes a changed model sampler conspicuous: linear sampling has
/// grey pixels between the texels, whereas nearest sampling can only choose black or white.
struct ContrastTexture;

impl CharacterLoader for ContrastTexture {
    fn load(&mut self, appearance: &PlayableModel) -> Result<Character, String> {
        let mut character = stick(*appearance, [255, 255, 255]);
        let mut quad = character.model.parts[0].clone();
        quad.positions = vec![[-6.0, 5.0, 0.0], [6.0, 5.0, 0.0], [6.0, -25.0, 0.0], [-6.0, -25.0, 0.0]];
        quad.uvs = vec![[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]];
        quad.normals = vec![[0.0; 3]; 4];
        quad.colors = vec![[255, 255, 255, 255]; 4];
        quad.indices = vec![0, 1, 2, 0, 2, 3];
        quad.texture = Some(Arc::new(RgbaImage { width: 2, height: 2,
            rgba: [[0, 0, 0, 255], [255, 255, 255, 255], [255, 255, 255, 255], [0, 0, 0, 255]].concat() }));
        quad.texture_name = Some("contrast".into());
        character.model.parts = vec![quad];
        Ok(character)
    }
}

fn textured_frame() -> AvatarFrame {
    let mut avatars = Avatars::new(AvatarLibrary::new(Box::new(ContrastTexture)));
    let player = facing_camera(1, "ff7/barret", Pos::new(0.0, 0.0, 0.0));
    let mut frame = avatars.frame(&view(), &stage(), &[player], 0.0).frame;
    frame.ground.clear();
    assert_eq!(frame.models.len(), 1, "the synthetic textured model is drawable");
    frame
}

fn assert_textured_model_and_hud(image: &[u8]) {
    let grey = image.chunks_exact(4).filter(|p| p[0] == p[1] && p[1] == p[2] && (20..235).contains(&p[0])).count();
    assert!(grey > 100, "the model texture visibly retains interpolation: {grey} grey pixels");
    assert_eq!(pixel(image, W, 9, 9), [8, 24, 80], "the native HUD's opaque panel is drawn");
    let mut white = 0;
    for y in 8..32 { for x in 8..264 {
        if pixel(image, W, x, y) == [255, 255, 255] { white += 1; }
    } }
    assert!(white > 30, "the native font is drawn above the scene");
}

#[test]
#[ignore = "needs a GPU or software Vulkan/GL driver"]
fn offscreen_background_filter_leaves_cached_and_new_field_and_battle_model_textures_and_hud_unchanged() {
    let mut gpu = Offscreen::new().expect("GPU or software adapter");
    let mut frame = textured_frame();
    // A solid tile exercises the scenery sampler alongside the model, while keeping the background
    // identical under either filter so a whole-image comparison isolates models and native HUD.
    frame.scenery = Some(scenery_of(&[solid(2, (-16, -16), 32, DEPTH_FAR, GREEN, None)], canvas()));
    let mut overlay = DrawList::new();
    overlay.fill_rect(8, 8, 256, 24, [8, 24, 80, 255]);
    overlay.draw_text(12, 16, 1, [255, 255, 255, 255], "MODEL TEXTURE / NATIVE HUD");
    let linear = filtered_field(&mut gpu, BackgroundFilter::Linear, &scene(), &frame, &overlay);
    assert_textured_model_and_hud(&linear);
    assert_eq!(gpu.cached_avatar_models(), 1);
    assert_eq!(linear, filtered_field(&mut gpu, BackgroundFilter::Nearest, &scene(), &frame, &overlay),
        "a cached field model and native HUD keep every pixel");
    let mut fresh = textured_frame();
    assert_ne!(frame.models[0].asset.id, fresh.models[0].asset.id);
    fresh.scenery = frame.scenery.clone();
    let nearest_new = filtered_field(&mut gpu, BackgroundFilter::Nearest, &scene(), &fresh, &overlay);
    assert_eq!(gpu.cached_avatar_models(), 2, "a new model was uploaded after the Nearest switch");
    assert_eq!(linear, nearest_new, "new field models still use their own linear sampler");

    // Battles use the same model renderer but a separate scene path. Check both the existing model
    // and a model first uploaded while the background preference is Nearest.
    frame.scenery = None;
    let mut battle = BattleFrame { clear_rgb: CLEAR_RGB, stage: Vec::new(), location: None,
        models: frame, tints: Vec::new(), identities: Vec::new(), camera: fitted() };
    let render_battle = |gpu: &mut Offscreen, filter, battle: &BattleFrame| {
        gpu.render_with(W, H, |gpu, target, srgb| {
            gpu.set_background_filter(filter);
            gpu.draw_battle(target, (W, H), battle, srgb);
            gpu.draw_overlay(target, &overlay.vertices(W, H, srgb));
        })
    };
    let battle_linear = render_battle(&mut gpu, BackgroundFilter::Linear, &battle);
    assert_textured_model_and_hud(&battle_linear);
    assert_eq!(battle_linear, render_battle(&mut gpu, BackgroundFilter::Nearest, &battle));
    battle.models = textured_frame();
    assert_eq!(battle_linear, render_battle(&mut gpu, BackgroundFilter::Nearest, &battle),
        "new battle models keep their texture filter too");
    assert_eq!(gpu.cached_avatar_models(), 3, "the new battle model exercises another texture upload");
    assert_eq!(battle_linear, render_battle(&mut gpu, BackgroundFilter::Linear, &battle));
    save("background-filter-model-and-native-hud", W, H, &linear);
    save("background-filter-battle-model-and-native-hud", W, H, &battle_linear);
}
