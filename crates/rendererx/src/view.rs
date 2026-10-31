//! A view: one window's (or thumbnail's) render target, camera uniforms and the key of
//! the last frame it drew.

use crate::{Camera, RenderOptions};

/// Colour target format: RGBA8, sRGB-encoded by the shader, straight alpha. Plain
/// `Unorm` so the host samples the encoded bytes as-is (libgui's texture contract).
pub const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
pub(crate) const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// What frames are drawn into with MSAA: sRGB, so samples are resolved in linear light.
pub(crate) const LINEAR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
pub(crate) const UNIFORM_BYTES: u64 = 24 * 4;

#[derive(Debug, Clone, Copy)]
pub struct ViewDesc {
    pub width: u32,
    pub height: u32,
}

pub(crate) struct Msaa {
    /// Multisampled colour, drawn in linear light.
    pub samples: wgpu::TextureView,
    /// Single-sampled sRGB resolve, encoded into the target by a final pass.
    pub resolve: wgpu::TextureView,
    pub encode: wgpu::BindGroup,
}

pub(crate) struct Targets {
    pub color: wgpu::Texture,
    pub color_view: wgpu::TextureView,
    pub msaa: Option<Msaa>,
    pub depth: wgpu::TextureView,
    /// The target and (with MSAA) the resolve texture: always backed by memory.
    pub bytes: u64,
    /// MSAA samples and depth: transient attachments, which take no memory on
    /// tile-based GPUs (Apple, most mobile) and full memory elsewhere.
    pub transient_bytes: u64,
}

/// What a frame depended on. The same key means the same pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct FrameKey {
    pub scene: u64,
    pub generation: u64,
    /// The renderer's compiled-pipeline count: a frame drawn with clay stand-ins is
    /// stale once the real pipeline is ready.
    pub pipelines: u64,
    pub camera: Camera,
    pub options: RenderOptions,
    pub size: (u32, u32),
}

pub struct View {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) targets: Option<Targets>,
    pub(crate) uniforms: wgpu::Buffer,
    pub(crate) bind_group: wgpu::BindGroup,
    pub(crate) last: Option<FrameKey>,
    /// Bumped whenever the target texture is recreated, so hosts know to re-register it.
    pub(crate) target_generation: u64,
}

impl View {
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// The new size takes effect on the next render, which recreates the targets.
    pub fn resize(&mut self, width: u32, height: u32) {
        let (width, height) = (width.max(1), height.max(1));
        if (width, height) != (self.width, self.height) {
            self.width = width;
            self.height = height;
            self.release_targets();
        }
    }

    /// Frees the window-sized textures (a hidden or minimised window). The next render
    /// recreates them.
    pub fn release_targets(&mut self) {
        if self.targets.take().is_some() {
            self.last = None;
        }
    }

    /// The finished frame, once one has been rendered. Recreated on resize; compare
    /// [`target_generation`](Self::target_generation) to notice.
    pub fn target(&self) -> Option<&wgpu::Texture> {
        self.targets.as_ref().map(|t| &t.color)
    }

    pub fn target_view(&self) -> Option<&wgpu::TextureView> {
        self.targets.as_ref().map(|t| &t.color_view)
    }

    pub fn target_generation(&self) -> u64 {
        self.target_generation
    }

    /// GPU bytes held for sure: the target (plus the MSAA resolve) and uniforms. The
    /// MSAA and depth attachments are counted separately in
    /// [`transient_bytes`](Self::transient_bytes).
    pub fn gpu_bytes(&self) -> u64 {
        self.targets.as_ref().map_or(0, |t| t.bytes) + UNIFORM_BYTES
    }

    /// The most the transient attachments (MSAA samples, depth) can take: zero on
    /// tile-based GPUs that keep them on chip, this much elsewhere.
    pub fn transient_bytes(&self) -> u64 {
        self.targets.as_ref().map_or(0, |t| t.transient_bytes)
    }

    /// Copies the last frame back to the CPU as tightly packed RGBA8 rows. Blocks until
    /// the GPU is done: for thumbnails, screenshots and tests, not for every frame.
    pub fn read_pixels(&self, device: &wgpu::Device, queue: &wgpu::Queue) -> Option<Vec<u8>> {
        let target = self.target()?;
        let (w, h) = (self.width, self.height);
        let row = w * 4;
        let padded = row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rendererx readback"),
            size: padded as u64 * h as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: Some(h) },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        let index = queue.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        buffer.map_async(wgpu::MapMode::Read, .., move |r| {
            let _ = tx.send(r);
        });
        device.poll(wgpu::PollType::Wait { submission_index: Some(index), timeout: None }).ok()?;
        rx.recv().ok()?.ok()?;
        let mapped = buffer.get_mapped_range(..).ok()?;
        let mut out = Vec::with_capacity((row * h) as usize);
        for y in 0..h as usize {
            let start = y * padded as usize;
            out.extend_from_slice(&mapped[start..start + row as usize]);
        }
        Some(out)
    }
}
