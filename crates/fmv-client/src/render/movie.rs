//! Bounded movie-frame presentation, independent of the movie clock and field-script VM.
//!
//! Upload binds an explicit stream and exact decoder frame/PTS identity. A later ordinal
//! reuses the texture; a wrong stream, changed metadata or stale frame cannot replace it.
//! A single completion-guarded pixel upload applies nonblocking backpressure if the GPU
//! falls behind, bounding staging memory as well as the retained texture.
//! Replacing a stream and clearing its allocation are explicit operations. Decoded pixels
//! are borrowed for upload, never retained on the CPU by this primitive.
//!
//! The caller supplies display aspect and fitting policy. Encoded width/height describes
//! storage, not proven FF7 display geometry. In particular, 1280x896 alone cannot decide
//! whether an opening should be drawn at 10:7, 4:3, cropped or stretched.
//!
//! `draw_movie_base` clears black and draws only the movie. Field shapes/models and UI
//! overlays are separate subsequent calls, so neither overlay/camera alignment nor live
//! movie registration is implied. Its result means GPU work was submitted, not displayed
//! by a surface, heard by an audio device, or acknowledged to a playback timeline.

// Owned-media evidence, not an automatic runtime policy: ffprobe on the owner's Steam
// opening.avi reports 1280x896, SAR 1:1 and DAR 10:7. The fmv-media metadata profile deliberately
// does not retain SAR/DAR, so a caller must pass its reviewed aspect explicitly. FFNx's pinned
// ordinary path corroborates proportional 640x448 inside a centered 640x480 game viewport:
// https://github.com/julianxhokaxhiu/FFNx/blob/661f92a36c3f1d6498317e04327c31dc24d06222/src/gl/gl.cpp#L57-L126
// That is FFNx evidence, not proof of original PC overlay/camera alignment. A full-window
// 10:7 containment and a 10:7 movie inside a 4:3 inner viewport remain separate caller choices.

use super::{backdrop_bind_group, Gpu, QuadVertex};
use fmv_media::{DecodedFrame, FrameTiming, TimeBase, VideoMetadata};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub const MAX_MOVIE_SIDE: u32 = 4096;
pub const MAX_MOVIE_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_MOVIE_TIMINGS: usize = 100_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MovieError {
    Geometry,
    FrameBytes,
    Timing,
    FrameIdentity,
    WrongStream,
    ChangedMetadata,
    StaleFrame,
    UploadBusy,
    DeviceProgress,
    NoFrame,
    DisplayPolicy,
}

impl std::fmt::Display for MovieError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "movie presentation rejected: {self:?}") }
}
impl std::error::Error for MovieError {}

/// An explicit display ratio, independent of the encoded pixel dimensions. No default
/// selects a movie's display geometry on behalf of the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayAspect {
    width: u32,
    height: u32,
}
impl DisplayAspect {
    pub fn new(width: u32, height: u32) -> Result<Self, MovieError> {
        if width == 0 || height == 0 { return Err(MovieError::DisplayPolicy); }
        Ok(Self { width, height })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MovieFit {
    /// Retain the complete image at the supplied display aspect; unused space is black.
    Contain,
    /// Fill the target at the supplied display aspect, cropping equal amounts at its edges.
    Cover,
    /// Explicitly map the complete image to the target shape, disregarding display aspect.
    Stretch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayPolicy {
    pub aspect: DisplayAspect,
    pub fit: MovieFit,
}

/// Destination game/movie viewport inside the target, in pixels from its top-left.
/// Keeping this separate from display aspect lets a caller explicitly retain an outer
/// 4:3 game viewport on a wide window without making that decision in the video renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MovieViewport {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}
impl MovieViewport {
    pub fn full(size: (u32, u32)) -> Self { Self { x: 0, y: 0, width: size.0, height: size.1 } }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MovieFrameId {
    pub stream: u64,
    pub ordinal: usize,
    pub timing: FrameTiming,
    pub time_base: TimeBase,
}

/// The base pass was enqueued. This is deliberately not a presentation acknowledgment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MovieSubmission {
    pub frame: MovieFrameId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MovieState {
    pub allocations: u64,
    pub uploads: u64,
    pub geometry: Option<(u32, u32)>,
    pub frame: Option<MovieFrameId>,
}

struct MovieTexture {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    geometry: (u32, u32),
}

/// One texture and its current identity. The metadata fingerprint binds timing and geometry
/// without retaining another copy of the decoder's timing table or any decoded frames.
#[derive(Default)]
pub(super) struct MovieFrames {
    texture: Option<MovieTexture>,
    stream: Option<u64>,
    metadata: Option<[u8; 32]>,
    frame: Option<MovieFrameId>,
    allocations: u64,
    uploads: u64,
    /// One outstanding queue.write_texture staging allocation, independent of the current
    /// stream. Clear/replace must not reset this guard before GPU completion releases it.
    upload_in_flight: Arc<AtomicBool>,
}

fn validate_frame(stream: u64, metadata: &VideoMetadata, frame: &DecodedFrame, device_limit: u32)
    -> Result<(MovieFrameId, [u8; 32]), MovieError> {
    let (width, height) = (metadata.width, metadata.height);
    if width == 0 || height == 0 || width > MAX_MOVIE_SIDE || height > MAX_MOVIE_SIDE
        || width > device_limit || height > device_limit { return Err(MovieError::Geometry); }
    let bytes = metadata.frame_bytes().map_err(|_| MovieError::Geometry)?;
    if bytes > MAX_MOVIE_BYTES || bytes != frame.rgba.len() { return Err(MovieError::FrameBytes); }
    if metadata.frames.is_empty() || metadata.frames.len() > MAX_MOVIE_TIMINGS
        || metadata.time_base.numerator == 0 || metadata.time_base.denominator == 0
        || metadata.frames[0].pts != 0 { return Err(MovieError::Timing); }
    let mut hash = blake3::Hasher::new();
    hash.update(b"fmv-movie-texture-contract-v1");
    for number in [width, height, metadata.time_base.numerator, metadata.time_base.denominator] {
        hash.update(&number.to_le_bytes());
    }
    hash.update(&(metadata.frames.len() as u64).to_le_bytes());
    let mut previous = None;
    for timing in &metadata.frames {
        if timing.pts < 0 || timing.duration <= 0 || timing.pts.checked_add(timing.duration).is_none()
            || previous.is_some_and(|pts| timing.pts <= pts) { return Err(MovieError::Timing); }
        previous = Some(timing.pts);
        hash.update(&timing.pts.to_le_bytes());
        hash.update(&timing.duration.to_le_bytes());
    }
    if metadata.frames.get(frame.ordinal) != Some(&frame.timing) { return Err(MovieError::FrameIdentity); }
    let identity = MovieFrameId { stream, ordinal: frame.ordinal, timing: frame.timing, time_base: metadata.time_base };
    Ok((identity, *hash.finalize().as_bytes()))
}

/// A complete quad with top-left UV origin. Cover crops texture coordinates, keeping NDC
/// vertices bounded; Contain adds black space and Stretch is an explicit full-target choice.
fn movie_quad(size: (u32, u32), policy: DisplayPolicy) -> Result<[QuadVertex; 6], MovieError> {
    if size.0 == 0 || size.1 == 0 { return Err(MovieError::DisplayPolicy); }
    let target = f64::from(size.0) / f64::from(size.1);
    let display = f64::from(policy.aspect.width) / f64::from(policy.aspect.height);
    let (mut x, mut y, mut u, mut v) = (1.0f64, 1.0f64, 1.0f64, 1.0f64);
    match policy.fit {
        MovieFit::Contain if display > target => y = target / display,
        MovieFit::Contain => x = display / target,
        MovieFit::Cover if display > target => u = target / display,
        MovieFit::Cover => v = display / target,
        MovieFit::Stretch => {},
    }
    let (x, y, u0, u1, v0, v1) = (x as f32, y as f32, ((1.0 - u) * 0.5) as f32,
        ((1.0 + u) * 0.5) as f32, ((1.0 - v) * 0.5) as f32, ((1.0 + v) * 0.5) as f32);
    let corner = |pos, uv| QuadVertex { pos, uv };
    Ok([corner([-x, y], [u0, v0]), corner([-x, -y], [u0, v1]), corner([x, -y], [u1, v1]),
        corner([-x, y], [u0, v0]), corner([x, -y], [u1, v1]), corner([x, y], [u1, v0])])
}

fn movie_quad_in(size: (u32, u32), viewport: MovieViewport, policy: DisplayPolicy) -> Result<[QuadVertex; 6], MovieError> {
    if size.0 == 0 || size.1 == 0 || viewport.width == 0 || viewport.height == 0
        || viewport.x.checked_add(viewport.width).is_none_or(|right| right > size.0)
        || viewport.y.checked_add(viewport.height).is_none_or(|bottom| bottom > size.1) { return Err(MovieError::DisplayPolicy); }
    let mut quad = movie_quad((viewport.width, viewport.height), policy)?;
    let (width, height) = (f64::from(size.0), f64::from(size.1));
    let centre_x = (2.0 * f64::from(viewport.x) + f64::from(viewport.width)) / width - 1.0;
    let centre_y = 1.0 - (2.0 * f64::from(viewport.y) + f64::from(viewport.height)) / height;
    for vertex in &mut quad {
        vertex.pos = [(centre_x + f64::from(vertex.pos[0]) * f64::from(viewport.width) / width) as f32,
            (centre_y + f64::from(vertex.pos[1]) * f64::from(viewport.height) / height) as f32];
    }
    Ok(quad)
}

impl Gpu {
    /// Upload an exact decoder frame. The first successful upload binds its stream and
    /// metadata; later frames must advance ordinal within that same immutable contract.
    /// A redraw uses `draw_movie_base` without re-uploading. Every rejection leaves the
    /// previous valid texture, identity, allocation and upload counters unchanged. At most
    /// one movie pixel upload is in flight: UploadBusy is nonblocking backpressure, and the
    /// caller retains or drops its decoded frame according to its own playback timeline.
    pub fn upload_movie_frame(&mut self, stream: u64, metadata: &VideoMetadata, frame: &DecodedFrame)
        -> Result<MovieFrameId, MovieError> {
        if self.movie.stream.is_some_and(|current| current != stream) { return Err(MovieError::WrongStream); }
        let (identity, fingerprint) = validate_frame(stream, metadata, frame, self.device.limits().max_texture_dimension_2d)?;
        if self.movie.metadata.is_some_and(|current| current != fingerprint) { return Err(MovieError::ChangedMetadata); }
        if self.movie.frame.is_some_and(|current| identity.ordinal <= current.ordinal) { return Err(MovieError::StaleFrame); }
        // write_texture allocates staging memory for every call. Submitting immediately
        // avoids an unsubmitted backlog, and this completion guard bounds GPU lag too.
        // Poll once without waiting so an idle caller can observe completed work.
        self.device.poll(wgpu::PollType::Poll).map_err(|_| MovieError::DeviceProgress)?;
        if self.movie.upload_in_flight.load(Ordering::Acquire) { return Err(MovieError::UploadBusy); }
        let geometry = (metadata.width, metadata.height);
        if self.movie.texture.as_ref().is_none_or(|texture| texture.geometry != geometry) {
            let format = if self.format.is_srgb() { wgpu::TextureFormat::Rgba8UnormSrgb } else { wgpu::TextureFormat::Rgba8Unorm };
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("fmv-movie-frame"),
                size: wgpu::Extent3d { width: geometry.0, height: geometry.1, depth_or_array_layers: 1 },
                mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2, format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST, view_formats: &[],
            });
            // Independent of the field-background filter setting; a video has its own sampling.
            let sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("fmv-movie-sampler"), min_filter: wgpu::FilterMode::Linear, mag_filter: wgpu::FilterMode::Linear,
                address_mode_u: wgpu::AddressMode::ClampToEdge, address_mode_v: wgpu::AddressMode::ClampToEdge,
                ..wgpu::SamplerDescriptor::default()
            });
            let bind_group = backdrop_bind_group(&self.device, &self.quad_pipeline, &texture, &sampler);
            self.movie.texture = Some(MovieTexture { texture, bind_group, geometry });
            self.movie.allocations = self.movie.allocations.saturating_add(1);
        }
        let texture = &self.movie.texture.as_ref().expect("validated frame has a texture").texture;
        self.movie.upload_in_flight.store(true, Ordering::Release);
        self.queue.write_texture(wgpu::TexelCopyTextureInfo { texture, mip_level: 0, origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All }, &frame.rgba,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(geometry.0 * 4), rows_per_image: Some(geometry.1) },
            wgpu::Extent3d { width: geometry.0, height: geometry.1, depth_or_array_layers: 1 });
        self.queue.submit(std::iter::empty::<wgpu::CommandBuffer>());
        let pending = Arc::clone(&self.movie.upload_in_flight);
        self.queue.on_submitted_work_done(move || pending.store(false, Ordering::Release));
        self.movie.stream = Some(stream);
        self.movie.metadata = Some(fingerprint);
        self.movie.frame = Some(identity);
        self.movie.uploads = self.movie.uploads.saturating_add(1);
        Ok(identity)
    }

    /// Explicitly replace the stream. A compatible texture allocation may be reused, but no
    /// previous frame remains drawable while the new stream awaits its first valid upload.
    pub fn replace_movie_stream(&mut self, stream: u64) {
        self.movie.stream = Some(stream);
        self.movie.metadata = None;
        self.movie.frame = None;
    }

    /// Release the retained texture and identity. Diagnostic counters are lifetime totals.
    pub fn clear_movie(&mut self) {
        self.movie.texture = None;
        self.movie.stream = None;
        self.movie.metadata = None;
        self.movie.frame = None;
    }

    pub fn movie_state(&self) -> MovieState {
        MovieState { allocations: self.movie.allocations, uploads: self.movie.uploads,
            geometry: self.movie.texture.as_ref().map(|texture| texture.geometry), frame: self.movie.frame }
    }

    /// Clear black and submit the movie as the base layer. Field overlays must be drawn with
    /// explicit non-clearing passes afterward, then UI with `draw_overlay`. Calling a clearing
    /// `draw_field` afterward would replace the movie, not composite it. This method never
    /// acknowledges a decoded frame to PlaybackTimeline: queue submission is not display.
    pub fn draw_movie_base(&mut self, target: &wgpu::TextureView, size: (u32, u32), policy: DisplayPolicy)
        -> Result<MovieSubmission, MovieError> {
        self.draw_movie_base_in(target, size, MovieViewport::full(size), policy)
    }

    /// The same base pass with an explicitly selected inner game/movie viewport. The target
    /// outside it is black. This does not alter the caller's field camera or overlay mapping.
    pub fn draw_movie_base_in(&mut self, target: &wgpu::TextureView, size: (u32, u32), viewport: MovieViewport, policy: DisplayPolicy)
        -> Result<MovieSubmission, MovieError> {
        let frame = self.movie.frame.ok_or(MovieError::NoFrame)?;
        let texture = self.movie.texture.as_ref().ok_or(MovieError::NoFrame)?;
        let quad = movie_quad_in(size, viewport, policy)?;
        self.queue.write_buffer(&self.quad_buf, 0, bytemuck::cast_slice(&quad));
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("fmv-movie-base") });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("fmv-movie-base-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: target, depth_slice: None, resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store } })],
                depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: None, multiview_mask: None,
            });
            pass.set_pipeline(&self.quad_pipeline);
            pass.set_bind_group(0, &texture.bind_group, &[]);
            pass.set_vertex_buffer(0, self.quad_buf.slice(..));
            pass.draw(0..6, 0..1);
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        Ok(MovieSubmission { frame })
    }
}

#[cfg(test)]
mod tests;
