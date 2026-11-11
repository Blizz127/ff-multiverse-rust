//! Authored background state lives with the field, independently of a scene player's lifetime.
//! Both rendering paths select the same masks. In particular, hidden animation variants must
//! be removed before Scenery bakes occluded texels into its atlas.

use super::{Backdrop, Scenery, Stage};
use crate::background_pack::FieldTextureSet;
use crate::scene::SceneBackground;
use fmv_ff7::background::{decode_background_with_palette, BackgroundOp, BackgroundOptions, BackgroundState, GroupFilter, PaletteState, ScreenRect};
use fmv_ff7::background::depth::{decode_background_tiles_with_depth_palette, DepthTile};
use fmv_ff7::background::raster::{compose_tile_pixels, TilePixels, MAX_RASTER_BYTES};
use fmv_ff7::field::{FieldFile, Palette};
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

const CACHED_FRAMES: usize = 4;
/// An optional high-resolution frame is larger than its native equivalent. Keep the
/// existing four-frame ceiling and also bound all cached pixel buffers together.
const MAX_CACHED_FRAME_BYTES: usize = 128 * 1024 * 1024;

pub(super) struct Frame {
    pub(super) backdrop: Backdrop,
    pub(super) scenery: Option<Arc<Scenery>>,
    pub(super) note: String,
}

impl Frame {
    fn pixel_bytes(&self) -> usize {
        self.backdrop.rgba.len().saturating_add(self.scenery.as_ref().map_or(0, |scenery| scenery.atlas.len()))
    }
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
    /// Immutable external textures loaded once at field entry. A palette change or a
    /// missing source page selects the native flat picture and depth atlas together.
    textures: Option<FieldTextureSet>,
    /// The actual GPU cap, or the conservative downlevel cap before a device exists.
    texture_limit: u32,
    scenery: bool,
    state: BackgroundState,
    palette: PaletteState,
    shown: Selection,
    failed: Option<Selection>,
    cache: VecDeque<(Selection, Frame)>,
}

impl DynamicBackground {
    #[cfg(test)]
    pub(super) fn new(key: &str, field: &FieldFile, canvas: ScreenRect, scenery: bool, initial: BackgroundState) -> Result<Self, String> {
        Self::new_with_textures(key, field, canvas, scenery, initial, None)
    }

    pub(super) fn new_with_textures(key: &str, field: &FieldFile, canvas: ScreenRect, scenery: bool, initial: BackgroundState,
        textures: Option<FieldTextureSet>) -> Result<Self, String> {
        let palette = field.palette().map_err(|e| e.to_string())?;
        let shown = Selection { groups: initial.clone(), palette: palette.clone() };
        Ok(Self { key: key.into(), field: field.clone(), canvas, tiles: None, textures, texture_limit: 2048, scenery,
            state: initial, palette: PaletteState::new(palette), shown, failed: None, cache: VecDeque::new() })
    }

    /// Initial and authored-animation frames share one selection/composition path.
    pub(super) fn initial_frame(&mut self) -> Result<Frame, String> { self.build() }

    /// Re-evaluate both render paths together when a renderer supplies its texture cap.
    /// Cached frames were built for the previous cap and cannot cross this boundary.
    pub(super) fn with_texture_limit(&mut self, limit: u32) -> Result<Option<Frame>, String> {
        let limit = limit.min(fmv_ff7::background::raster::MAX_RASTER_SIDE);
        if self.texture_limit == limit || self.textures.is_none() { return Ok(None); }
        let previous = std::mem::replace(&mut self.texture_limit, limit);
        match self.build() {
            Ok(frame) => {
                self.cache.clear();
                self.failed = None;
                self.shown = Selection { groups: self.state.clone(), palette: self.palette.palette().clone() };
                Ok(Some(frame))
            },
            Err(error) => { self.texture_limit = previous; Err(error) },
        }
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
            self.cache.push_front((self.shown.clone(), Frame { backdrop, scenery: old_scenery, note: String::new() }));
            self.cache.truncate(CACHED_FRAMES);
            while self.cache.iter().map(|(_, frame)| frame.pixel_bytes()).sum::<usize>() > MAX_CACHED_FRAME_BYTES {
                self.cache.pop_back();
            }
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
        if (self.scenery || self.textures.is_some()) && self.tiles.as_ref().is_none_or(|(old, _)| old != palette) {
            let tiles = decode_background_tiles_with_depth_palette(&self.field, palette,
                &BackgroundOptions { groups: GroupFilter::All, ..BackgroundOptions::default() }).map_err(|e| e.to_string())?;
            self.tiles = Some((palette.clone(), tiles));
        }
        let selected: Vec<_> = self.tiles.as_ref().into_iter().flat_map(|(_, tiles)| tiles)
            .filter(|tile| self.state.visible(tile.layer, tile.param, tile.state)).cloned().collect();
        if let Some(frame) = self.textures.as_ref().and_then(|textures|
            replacement_frame(&self.field, textures, palette, &selected, self.canvas, self.scenery, self.texture_limit)) {
            return Ok(frame);
        }
        let (scenery, mut note) = if self.scenery {
            let (scenery, note) = Scenery::choose(&selected, self.canvas);
            (scenery.map(Arc::new), note)
        } else { (None, "characters are drawn as markers".to_string()) };
        if self.textures.is_some() {
            note.push_str("; native background for an unsupported replacement palette, frame or GPU texture limit");
        }
        Ok(Frame { backdrop, scenery, note })
    }
}

/// Decide before allocating scaled pixels whether BOTH textures fit the GPU. The atlas
/// uses Scenery's shelf order (large tiles, then small) and its final opaque black row.
fn replacement_fits_device(tiles: &[DepthTile], canvas: ScreenRect, scale: u32, scenery: bool, limit: u32) -> bool {
    if !matches!(scale, 1 | 2 | 4) { return false; }
    let limit = u64::from(limit);
    let (width, height) = (i64::from(canvas.max_x) - i64::from(canvas.min_x), i64::from(canvas.max_y) - i64::from(canvas.min_y));
    if width <= 0 || height <= 0 || width as u64 * u64::from(scale) > limit || height as u64 * u64::from(scale) > limit { return false; }
    if !scenery || !tiles.iter().any(|tile| matches!(tile.layer, 2 | 4)) { return true; }
    let mut atlas_height = 1u64; // the black row
    for logical_side in [32i64, 16] {
        let count = tiles.iter().filter(|tile| i64::from(tile.rect.max_x) - i64::from(tile.rect.min_x) == logical_side).count() as u64;
        if count == 0 { continue; }
        let side = logical_side as u64 * u64::from(scale);
        let columns = u64::from(crate::avatar::scenery::ATLAS_MAX_WIDTH) / side;
        let width = count.min(columns) * side;
        atlas_height += count.div_ceil(columns) * side;
        if width > limit || atlas_height > limit { return false; }
    }
    true
}

/// Crop the original source pages at the replacement raster scale. Native tile identities,
/// geometry, order, blending and animation groups remain the authority for drawing them.
fn replacement_frame(field: &FieldFile, textures: &FieldTextureSet, palette: &Palette, tiles: &[DepthTile],
    canvas: ScreenRect, prepare_scenery: bool, texture_limit: u32) -> Option<Frame> {
    let pages = textures.for_palette(palette)?;
    let scale = textures.scale();
    if !replacement_fits_device(tiles, canvas, scale, prepare_scenery, texture_limit) { return None; }
    let source = field.background().ok()?;
    let source: BTreeMap<_, _> = source.tiles.iter().map(|tile| ((tile.layer, tile.index), tile)).collect();
    let mut owned = Vec::with_capacity(tiles.len());
    let mut bytes = 0usize;
    for tile in tiles {
        let original = source.get(&(tile.layer, tile.index))?;
        if u32::from(original.tile_size) != tile.size() { return None; }
        let (page, x, y) = original.effective_source();
        let page = pages.page(page)?;
        if page.side != 256u32.checked_mul(scale)? { return None; }
        let side = tile.size().checked_mul(scale)?;
        let (x, y) = (u32::from(x).checked_mul(scale)?, u32::from(y).checked_mul(scale)?);
        if x.checked_add(side)? > page.side || y.checked_add(side)? > page.side { return None; }
        let length = usize::try_from(side).ok()?.checked_mul(side as usize)?.checked_mul(4)?;
        bytes = bytes.checked_add(length)?;
        if bytes > MAX_RASTER_BYTES { return None; }
        let mut rgba = Vec::with_capacity(length);
        for row in 0..side {
            let at = usize::try_from((y + row).checked_mul(page.side)?.checked_add(x)?).ok()?.checked_mul(4)?;
            rgba.extend_from_slice(page.rgba.get(at..at.checked_add(side as usize * 4)?)?);
        }
        owned.push((side, rgba));
    }
    let pixels: Vec<_> = owned.iter().map(|(side, rgba)| TilePixels { side: *side, rgba }).collect();
    let raster = compose_tile_pixels(tiles, &pixels, canvas, scale).ok()?;
    let bounds = raster.logical_bounds;
    let backdrop = Backdrop::new(raster.width, raster.height, raster.rgba,
        [bounds.min_x as f32, -(bounds.max_y as f32)], [bounds.max_x as f32, -(bounds.min_y as f32)])?;
    let (scenery, note) = if prepare_scenery {
        let (scenery, note) = Scenery::choose_with_pixels(tiles, &pixels, canvas, scale);
        // If the scaled atlas was refused, do not leave models over an unoccluded flat
        // replacement. Both paths fall back to the original pixels as one frame.
        if scenery.is_none() && tiles.iter().any(|tile| matches!(tile.layer, 2 | 4)) { return None; }
        (scenery.map(Arc::new), note)
    } else { (None, "characters are drawn as markers".to_string()) };
    Some(Frame { backdrop, scenery, note: format!("{scale}x field textures; {note}") })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::avatar::stage::StageCamera;
    use fmv_ff7::background::{decode_background_with, entry_background_ops, BackgroundOp, PaletteOp};

    #[test]
    fn the_gpu_cap_must_fit_the_canvas_and_the_atlas_including_its_black_row() {
        use crate::avatar::scenery::tests::tile;
        let canvas = ScreenRect { min_x: 0, min_y: 0, max_x: 256, max_y: 256 };
        let tile = tile(2, 100, (0, 0), 16, [1, 2, 3], None);
        // All tiles overlap on a small logical canvas. Texture packing still needs 32
        // rows of64 texels, plus the black row: the canvas alone fits a 2048 GPU.
        let tiles = vec![tile; 1024];
        assert!(!replacement_fits_device(&tiles, canvas, 4, true, 2048));
        assert!(replacement_fits_device(&tiles, canvas, 4, false, 2048));
        assert!(replacement_fits_device(&tiles, canvas, 4, true, 2049));
        let wide = ScreenRect { max_x: 624, ..canvas };
        assert!(!replacement_fits_device(&tiles, wide, 4, true, 2048));
        assert!(replacement_fits_device(&tiles, wide, 4, true, 4096));
    }

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
