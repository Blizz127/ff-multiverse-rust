//! A movie over the whole window: the windowed client's one frame of a playing [`MovieController`].
//!
//! The surface handling is `render_flat`'s (minimised, occluded, outdated and lost surfaces are a skipped frame: nothing
//! is drawn, so no presentation receipt is made and the movie's final frame is simply not acknowledged yet). The frame
//! comes from the controller's own upload and base-draw steps; the receipt is returned to the controller only after the
//! surface has been handed to the queue's `present`, which is this window's presentation boundary.
//! Until the first frame has been decoded the window is cleared to the flat view's background.

use super::movie::{DisplayAspect, DisplayPolicy, MovieError, MovieFit, MovieViewport};
use super::{FrameResult, Renderer};
use crate::movie::{ControllerError, MovieController};

impl Renderer {
    /// Draw one frame of `movie` over the whole window, contained at the movie's own coded aspect (the owned movies are
    /// 1280x896, 10:7; black bars fill the rest). `Err` is a movie problem (the caller gives the movie up);
    /// `Ok(FrameResult::Fatal)` is the renderer's.
    pub(crate) fn render_movie(&mut self, movie: &mut MovieController) -> Result<FrameResult, String> {
        let (w, h) = self.size;
        if w == 0 || h == 0 {
            return Ok(FrameResult::Skipped);
        }
        if self.needs_reconfigure {
            self.reconfigure();
        }
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) => f,
            wgpu::CurrentSurfaceTexture::Suboptimal(f) => {
                self.needs_reconfigure = true;
                f
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded | wgpu::CurrentSurfaceTexture::Validation => {
                return Ok(FrameResult::Skipped)
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.needs_reconfigure = true;
                return Ok(FrameResult::Skipped);
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                return Ok(match self.recreate_surface() {
                    Ok(()) => FrameResult::Skipped,
                    Err(e) => FrameResult::Fatal(e),
                });
            }
        };
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        // A new movie replaces whatever stream the GPU held; the same movie's later frames must not.
        let generation = movie.id().generation;
        if self.gpu.movie_state().frame.map(|f| f.stream) != Some(generation) {
            movie.bind_gpu(&mut self.gpu);
        }
        match movie.upload_due(&mut self.gpu) {
            Ok(_) | Err(ControllerError::PresentationBusy | ControllerError::Gpu(MovieError::UploadBusy)) => {}
            Err(e) => return Err(e.to_string()),
        }
        if self.gpu.movie_state().frame.is_none() {
            // The decoder has not delivered a frame yet (or the GPU has none of this movie's): a plain frame.
            self.gpu.draw_flat(&view, (w, h), &[], self.srgb);
            self.window.pre_present_notify();
            self.gpu.queue.present(frame);
            return Ok(FrameResult::Presented);
        }
        let (width, height) = movie.metadata().map_or((1, 1), |m| (m.video.width, m.video.height));
        let policy = DisplayPolicy { aspect: DisplayAspect::new(width, height).map_err(|e| e.to_string())?, fit: MovieFit::Contain };
        let ticket = movie.draw_base(&mut self.gpu, &view, (w, h), MovieViewport::full((w, h)), policy).map_err(|e| e.to_string())?;
        self.window.pre_present_notify();
        self.gpu.queue.present(frame);
        movie.acknowledge_presented(ticket).map_err(|e| e.to_string())?;
        Ok(FrameResult::Presented)
    }
}
