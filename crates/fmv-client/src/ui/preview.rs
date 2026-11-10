//! A cached, locally loaded preview of the body selected in character creation.
use crate::avatar::{frame::{skin, AvatarFrame, SkinnedModel}, identity::Identity, library::AvatarLibrary, source::LgpLoader};
use crate::ui::menu::{Menu, Screen};
use fmv_core::AnimState;

pub struct Preview { library: AvatarLibrary }

/// Horizontal margin alongside the creator form, in pixels.
pub fn margin(size: (u32, u32)) -> f32 {
    let scale = crate::ui::screens::ui_scale(size.0, size.1);
    let canvas = size.0 / scale;
    let panel = canvas.saturating_sub(8).min(440) * scale;
    (size.0.saturating_sub(panel) as f32) / 2.0
}

impl Preview {
    pub fn local() -> Option<Self> {
        let path = crate::avatar::source::remembered()?;
        Some(Self { library: AvatarLibrary::new(Box::new(LgpLoader::new(path))) })
    }

    pub fn frame(&mut self, menu: &Menu, size: (u32, u32)) -> Option<AvatarFrame> {
        // The form uses the whole width in compact windows; never draw over its controls.
        let margin = margin(size);
        if margin < 36.0 || size.1 < 500 { return None; }
        let Screen::Create(form) = menu.screen() else { return None };
        let choice = menu.bodies().get(form.body)?.id;
        let body = fmv_ff7::playable::resolve_body(choice)?;
        let asset = self.library.get(Some(body.model.appearance_ref)).asset?;
        let pose = asset.character.pose(AnimState::Idle, 0)?;
        let (lo, hi) = asset.character.model.posed_bounds(&pose, &fmv_ff7::model::mat4_identity())?;
        let width = (hi[0] - lo[0]).max(1.0);
        let height = (hi[1] - lo[1]).max(1.0);
        let scale = ((margin - 12.0).min(100.0) / width).min(190.0 / height);
        let cx = (lo[0] + hi[0]) * 0.5;
        let cy = (lo[1] + hi[1]) * 0.5;
        let cz = (lo[2] + hi[2]) * 0.5;
        let place = [scale, 0.0, 0.0, 0.0, 0.0, scale, 0.0, 0.0, 0.0, 0.0, scale, 0.0,
                     -cx * scale, -cy * scale, -cz * scale, 1.0];
        let camera = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1000.0, 1.0];
        let mut positions = Vec::new();
        if !skin(&asset, &pose, &place, &camera, &mut positions) { return None; }
        Some(AvatarFrame { models: vec![SkinnedModel { asset, positions }], tints: vec![], identities: vec![Identity::of(form.tint)],
                           zoom: 1000.0, field_depth: None, overlay_bias: 0.12 * scale, ground: vec![], scenery: None })
    }
}
