//! Authored background state lives with the field, independently of a scene player's lifetime.
//! Both rendering paths select the same masks. In particular, hidden animation variants must
//! be removed before Scenery bakes occluded texels into its atlas.

use super::{Backdrop, Scenery, Stage};
use crate::scene::SceneBackground;
use fmv_ff7::background::{decode_background_with_palette, BackgroundOp, BackgroundOptions, BackgroundState, GroupFilter, PaletteState, ScreenRect};
use fmv_ff7::background::depth::{decode_background_tiles_with_depth_palette, DepthTile};
use fmv_ff7::field::{FieldFile, Palette};
use std::collections::VecDeque;
use std::sync::Arc;

const CACHED_FRAMES: usize = 4;

struct Frame {
    backdrop: Backdrop,
    scenery: Option<Arc<Scenery>>,
}

/// Only visible state identifies GPU resources. Scratch changes need no upload until LDPAL.
#[derive(Clone, PartialEq, Eq)]
struct Selection {
    groups: BackgroundState,
    palette: Palette,
}

pub(super) struct DynamicBackground {
    key: String,
    field: FieldFile,
    canvas: ScreenRect,
    /// Decode for the current palette, then select before atlas baking. Never draw all variants.
    tiles: Option<(Palette, Vec<DepthTile>)>,
    scenery: bool,
    state: BackgroundState,
    palette: PaletteState,
    shown: Selection,
    failed: Option<Selection>,
    cache: VecDeque<(Selection, Frame)>,
}

impl DynamicBackground {
    pub(super) fn new(key: &str, field: &FieldFile, canvas: ScreenRect, scenery: bool, initial: BackgroundState) -> Result<Self, String> {
        let palette = field.palette().map_err(|e| e.to_string())?;
        let shown = Selection { groups: initial.clone(), palette: palette.clone() };
        Ok(Self { key: key.into(), field: field.clone(), canvas, tiles: None, scenery,
            state: initial, palette: PaletteState::new(palette), shown, failed: None, cache: VecDeque::new() })
    }

    #[cfg(test)]
    pub(super) fn shown_state(&self) -> &BackgroundState { &self.shown.groups }

    #[cfg(test)]
    pub(super) fn shown_palette(&self) -> &Palette { &self.shown.palette }

    pub(super) fn apply(
        &mut self,
        events: impl IntoIterator<Item = SceneBackground>,
        backdrop: &mut Option<Backdrop>,
        stage: Option<&mut Stage>,
    ) -> Result<(), String> {
        // Refuse an invalid batch before changing either scratch or visible state. In particular
        // a malformed palette range cannot leave a partially copied buffer for a later upload.
        let mut next_groups = self.state.clone();
        let mut next_palette = self.palette.clone();
        for event in events {
            if event.field != self.key { continue; }
            match event.op {
                BackgroundOp::Palette(op) => { next_palette.apply(op)?; },
                op => { next_groups.apply(op); },
            }
        }
        self.state = next_groups;
        self.palette = next_palette;
        let selection = Selection { groups: self.state.clone(), palette: self.palette.palette().clone() };
        // A frame may contain several changes (off then on); only build its final selection.
        if selection == self.shown || self.failed.as_ref() == Some(&selection) { return Ok(()); }
        let next = match self.cache.iter().position(|(state, _)| *state == selection) {
            Some(index) => self.cache.remove(index).expect("the matching cached frame").1,
            None => match self.build() {
                Ok(frame) => frame,
                Err(error) => { self.failed = Some(selection); return Err(error); }
            },
        };
        let old_backdrop = backdrop.replace(next.backdrop);
        let old_scenery = stage.and_then(|stage| std::mem::replace(&mut stage.scenery, next.scenery));
        if let Some(backdrop) = old_backdrop {
            self.cache.push_front((self.shown.clone(), Frame { backdrop, scenery: old_scenery }));
            self.cache.truncate(CACHED_FRAMES);
        }
        self.shown = selection;
        self.failed = None;
        Ok(())
    }

    fn build(&mut self) -> Result<Frame, String> {
        let options = BackgroundOptions { groups: GroupFilter::State(self.state.clone()), ..BackgroundOptions::default() };
        let palette = self.palette.palette();
        let image = decode_background_with_palette(&self.field, palette, &options).map_err(|e| e.to_string())?;
        let backdrop = Backdrop::from_ff7(&image).ok_or("unusable authored background image")?;
        if self.scenery && self.tiles.as_ref().is_none_or(|(old, _)| old != palette) {
            let tiles = decode_background_tiles_with_depth_palette(&self.field, palette,
                &BackgroundOptions { groups: GroupFilter::All, ..BackgroundOptions::default() }).map_err(|e| e.to_string())?;
            self.tiles = Some((palette.clone(), tiles));
        }
        let scenery = self.tiles.as_ref().and_then(|(_, tiles)| {
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
    use fmv_ff7::background::{decode_background_with, entry_background_ops, BackgroundOp, PaletteOp};

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
        let mut stage = Stage { camera: StageCamera::from_camera(&field.cameras().unwrap()[0]), depth: Default::default(), model_scale: 1.0,
            walk_speed: 1.0, run_speed: 1.0, scenery };
        let mut background = DynamicBackground::new("tin_1", &field, canvas, true, initial.clone()).unwrap();
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
        let fresh = DynamicBackground::new("tin_1", &field, canvas, true, initial.clone()).unwrap();
        assert_eq!(fresh.shown_state(), &initial, "a fresh field visit reconstructs its original initializer");
    }

    #[test]
    #[ignore = "needs the owner's original FF7 field archive (FMV_FF7_DATA)"]
    fn authored_rootmap_palette_uploads_change_both_paths_without_resetting_scratch_between_scenes() {
        let Some(data) = std::env::var_os("FMV_FF7_DATA") else { eprintln!("FMV_FF7_DATA is needed; skipping"); return };
        let mut lgp = fmv_ff7::lgp::LgpArchive::open_path(std::path::PathBuf::from(data).join("field/flevel.lgp")).unwrap();
        let field = FieldFile::parse(fmv_ff7::lzss::decompress(&lgp.read("rootmap").unwrap()).unwrap()).unwrap();
        let mut groups = BackgroundState::default();
        groups.apply(BackgroundOp::On { group: 1, frame: 0 });
        let options = BackgroundOptions { groups: GroupFilter::State(groups.clone()), ..BackgroundOptions::default() };
        let original = decode_background_with(&field, &options).unwrap();
        let canvas = original.bounds();
        let mut backdrop = Some(Backdrop::from_ff7(&original).unwrap());
        let mut stage = Stage { camera: StageCamera::from_camera(&field.cameras().unwrap()[0]), depth: Default::default(),
            model_scale: 1.0, walk_speed: 1.0, run_speed: 1.0,
            scenery: Scenery::from_field_with_options(&field, canvas, &options).0.map(Arc::new) };
        let mut background = DynamicBackground::new("rootmap", &field, canvas, true, groups.clone()).unwrap();
        let ids = |backdrop: &Option<Backdrop>, stage: &Stage| (backdrop.as_ref().unwrap().id, stage.scenery.as_ref().unwrap().id);
        let initial_ids = ids(&backdrop, &stage);
        let command = |op| SceneBackground { field: "rootmap".into(), op: BackgroundOp::Palette(op) };
        // The original monitor stores palette 7 in scratch block 0, scales into block 8,
        // restores color zero with CPPAL's encoded count 0 (one color), then uploads page 0.
        background.apply([
            command(PaletteOp::Store { palette: 7, position: 0, count: 128 }),
            command(PaletteOp::Multiply { source: 0, destination: 8, red: 0, green: 0, blue: 0, count: 128 }),
            command(PaletteOp::Copy { source: 0, destination: 8, count: 1 }),
        ], &mut backdrop, Some(&mut stage)).unwrap();
        assert_eq!(ids(&backdrop, &stage), initial_ids, "scratch-only changes do not invalidate either texture");
        background.apply([command(PaletteOp::Load { position: 8, palette: 0, count: 128 })], &mut backdrop, Some(&mut stage)).unwrap();
        let dark_ids = ids(&backdrop, &stage);
        assert_ne!(dark_ids.0, initial_ids.0);
        assert_ne!(dark_ids.1, initial_ids.1);
        assert_eq!(background.palette.palette().rgba(0, 0).unwrap()[3], 0, "color zero retains PC transparency");
        let dark_pixels = backdrop.as_ref().unwrap().rgba.clone();
        let direct = decode_background_with_palette(&field, background.palette.palette(), &options).unwrap();
        assert_eq!(dark_pixels, direct.flatten_on_black());
        let direct_tiles = decode_background_tiles_with_depth_palette(&field, background.palette.palette(), &options).unwrap();
        let direct_scenery = Scenery::choose(&direct_tiles, canvas).0.unwrap();
        assert_eq!(stage.scenery.as_ref().unwrap().atlas, direct_scenery.atlas, "the depth cache cannot retain the previous palette");
        let original_tiles = decode_background_tiles_with_depth_palette(&field, &field.palette().unwrap(), &options).unwrap();
        assert_eq!(direct_tiles.iter().map(|t| (t.layer, t.index, t.depth, t.blend)).collect::<Vec<_>>(),
            original_tiles.iter().map(|t| (t.layer, t.index, t.depth, t.blend)).collect::<Vec<_>>(), "palette uploads preserve blending and tile order");
        // Ending a scene does not clear scratch; the next one can use its stored source.
        background.apply([], &mut backdrop, Some(&mut stage)).unwrap();
        background.apply([
            command(PaletteOp::Multiply { source: 0, destination: 8, red: 64, green: 64, blue: 64, count: 128 }),
            command(PaletteOp::Copy { source: 0, destination: 8, count: 1 }),
            command(PaletteOp::Load { position: 8, palette: 0, count: 128 }),
        ], &mut backdrop, Some(&mut stage)).unwrap();
        assert_ne!(backdrop.as_ref().unwrap().rgba, dark_pixels, "a later scene uses the same field's stored colors");
        assert_eq!(background.palette.palette().raw(0, 42), field.palette().unwrap().raw(7, 42));
        // Returning to an earlier visible palette reuses BOTH identities, despite scratch edits.
        background.apply([
            command(PaletteOp::Multiply { source: 0, destination: 8, red: 0, green: 0, blue: 0, count: 128 }),
            command(PaletteOp::Copy { source: 0, destination: 8, count: 1 }),
            command(PaletteOp::Load { position: 8, palette: 0, count: 128 }),
        ], &mut backdrop, Some(&mut stage)).unwrap();
        assert_eq!(ids(&backdrop, &stage), dark_ids);
        assert_eq!(backdrop.as_ref().unwrap().rgba, dark_pixels);
        let mut fresh = DynamicBackground::new("rootmap", &field, canvas, true, groups).unwrap();
        assert_eq!(fresh.palette.palette(), &field.palette().unwrap());
        fresh.palette.apply(PaletteOp::Load { position: 0, palette: 0, count: 128 }).unwrap();
        assert_eq!(fresh.palette.palette().raw(0, 42), Some(0), "a genuine new visit clears scratch");
    }
}
