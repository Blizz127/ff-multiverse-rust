//! Authored background state lives with the field, independently of a scene player's lifetime.
//! Both rendering paths select the same masks. In particular, hidden animation variants must
//! be removed before Scenery bakes occluded texels into its atlas.

use super::{Backdrop, Scenery, Stage};
use crate::scene::SceneBackground;
use fmv_ff7::background::{decode_background_with, BackgroundOptions, BackgroundState, GroupFilter, ScreenRect};
use fmv_ff7::background::depth::{decode_background_tiles_with_depth_options, DepthTile};
use fmv_ff7::field::FieldFile;
use std::collections::VecDeque;
use std::sync::Arc;

const CACHED_FRAMES: usize = 4;

struct Frame {
    backdrop: Backdrop,
    scenery: Option<Arc<Scenery>>,
}

pub(super) struct DynamicBackground {
    key: String,
    field: FieldFile,
    canvas: ScreenRect,
    /// Decoded once, selected before every atlas bake. Never draw all variants together.
    tiles: Option<Vec<DepthTile>>,
    state: BackgroundState,
    shown: BackgroundState,
    failed: Option<BackgroundState>,
    cache: VecDeque<(BackgroundState, Frame)>,
}

impl DynamicBackground {
    pub(super) fn new(key: &str, field: &FieldFile, canvas: ScreenRect, scenery: bool, initial: BackgroundState) -> Self {
        let tiles = scenery.then(|| decode_background_tiles_with_depth_options(field,
            &BackgroundOptions { groups: GroupFilter::All, ..BackgroundOptions::default() }).ok()).flatten();
        Self { key: key.into(), field: field.clone(), canvas, tiles, state: initial.clone(), shown: initial, failed: None, cache: VecDeque::new() }
    }

    #[cfg(test)]
    pub(super) fn shown_state(&self) -> &BackgroundState { &self.shown }

    pub(super) fn apply(
        &mut self,
        events: impl IntoIterator<Item = SceneBackground>,
        backdrop: &mut Option<Backdrop>,
        stage: Option<&mut Stage>,
    ) -> Result<(), String> {
        for event in events {
            if event.field == self.key { self.state.apply(event.op); }
        }
        // A frame may contain several changes (off then on); only build its final selection.
        if self.state == self.shown || self.failed.as_ref() == Some(&self.state) { return Ok(()); }
        let next = match self.cache.iter().position(|(state, _)| *state == self.state) {
            Some(index) => self.cache.remove(index).expect("the matching cached frame").1,
            None => match self.build() {
                Ok(frame) => frame,
                Err(error) => { self.failed = Some(self.state.clone()); return Err(error); }
            },
        };
        let old_backdrop = backdrop.replace(next.backdrop);
        let old_scenery = stage.and_then(|stage| std::mem::replace(&mut stage.scenery, next.scenery));
        if let Some(backdrop) = old_backdrop {
            self.cache.push_front((self.shown.clone(), Frame { backdrop, scenery: old_scenery }));
            self.cache.truncate(CACHED_FRAMES);
        }
        self.shown = self.state.clone();
        self.failed = None;
        Ok(())
    }

    fn build(&self) -> Result<Frame, String> {
        let options = BackgroundOptions { groups: GroupFilter::State(self.state.clone()), ..BackgroundOptions::default() };
        let image = decode_background_with(&self.field, &options).map_err(|e| e.to_string())?;
        let backdrop = Backdrop::from_ff7(&image).ok_or("unusable authored background image")?;
        let scenery = self.tiles.as_ref().and_then(|tiles| {
            let selected: Vec<_> = tiles.iter().filter(|t| self.state.visible(t.layer, t.param, t.state)).cloned().collect();
            Scenery::choose(&selected, self.canvas).0.map(Arc::new)
        });
        Ok(Frame { backdrop, scenery })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::avatar::stage::StageCamera;
    use fmv_ff7::background::{entry_background_ops, BackgroundOp};

    #[test]
    #[ignore = "needs the owner's original FF7 field archive (FMV_FF7_DATA)"]
    fn authored_frame_changes_update_both_render_paths_keep_their_identity_and_end_state() {
        let Some(data) = std::env::var_os("FMV_FF7_DATA") else { eprintln!("FMV_FF7_DATA is needed; skipping"); return };
        let mut lgp = fmv_ff7::lgp::LgpArchive::open_path(std::path::PathBuf::from(data).join("field/flevel.lgp")).unwrap();
        let field = FieldFile::parse(fmv_ff7::lzss::decompress(&lgp.read("tin_1").unwrap()).unwrap()).unwrap();
        let mut initial = BackgroundState::default();
        for op in entry_background_ops(&field, "tin_1").unwrap() { initial.apply(op); }
        let options = BackgroundOptions { groups: GroupFilter::State(initial.clone()), ..BackgroundOptions::default() };
        let image = decode_background_with(&field, &options).unwrap();
        let canvas = image.bounds();
        let mut backdrop = Some(Backdrop::from_ff7(&image).unwrap());
        let scenery = Scenery::from_field_with_options(&field, canvas, &options).0.map(Arc::new);
        let mut stage = Stage { camera: StageCamera::from_camera(&field.cameras().unwrap()[0]), model_scale: 1.0,
            walk_speed: 1.0, run_speed: 1.0, scenery };
        let mut background = DynamicBackground::new("tin_1", &field, canvas, true, initial.clone());
        let ids = |backdrop: &Option<Backdrop>, stage: &Stage| (backdrop.as_ref().unwrap().id, stage.scenery.as_ref().unwrap().id);
        let closed = ids(&backdrop, &stage);
        let closed_pixels = backdrop.as_ref().unwrap().rgba.clone();
        let op = |op| SceneBackground { field: "tin_1".into(), op };
        background.apply([
            op(BackgroundOp::Off { group: 6, frame: 1 }),
            op(BackgroundOp::On { group: 6, frame: 2 }),
        ], &mut backdrop, Some(&mut stage)).unwrap();
        let opened = ids(&backdrop, &stage);
        assert_ne!(opened.0, closed.0, "flat texture must be uploaded for the changed selection");
        assert_ne!(opened.1, closed.1, "the depth atlas must change with the flat picture");
        assert_ne!(backdrop.as_ref().unwrap().rgba, closed_pixels);
        // Compare the cached-all-tiles/filter path to an independent direct selected decoder.
        let selected = BackgroundOptions { groups: GroupFilter::State(background.state.clone()), ..BackgroundOptions::default() };
        let direct = Scenery::from_field_with_options(&field, canvas, &selected).0.unwrap();
        assert_eq!(stage.scenery.as_ref().unwrap().atlas, direct.atlas, "hidden variants cannot contaminate baked texels");
        assert_eq!(stage.scenery.as_ref().unwrap().opaque.len(), direct.opaque.len());
        // No active scene after its terminal event: the field keeps that event, unchanged.
        background.apply([], &mut backdrop, Some(&mut stage)).unwrap();
        background.apply([SceneBackground { field: "cargoin".into(), op: BackgroundOp::Clear { group: 6 } }], &mut backdrop, Some(&mut stage)).unwrap();
        assert_eq!(ids(&backdrop, &stage), opened, "empty and old-field queues leave the final selection in place");
        background.apply([op(BackgroundOp::Off { group: 6, frame: 2 }), op(BackgroundOp::On { group: 6, frame: 1 })], &mut backdrop, Some(&mut stage)).unwrap();
        assert_eq!(ids(&backdrop, &stage), closed, "returning to a cached selection reuses both GPU identities");
        assert_eq!(backdrop.as_ref().unwrap().rgba, closed_pixels);
        let fresh = DynamicBackground::new("tin_1", &field, canvas, true, initial.clone());
        assert_eq!(fresh.shown_state(), &initial, "a fresh field visit reconstructs its original initializer");
    }
}
