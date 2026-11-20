//! libviewer's [`Host`] on a wgpu device: image tiles as textures, and RendererX
//! for 3D scenes with its pipelines compiled on a thread.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context as _;
use libgui::TextureId;
use libviewer::{Frame3d, Host, SceneGpu, draw_scene};
use rendererx::Camera;
use rendererx::scene_data::CpuScene;

/// Wakes the shell from another thread: a texture decoded, a pipeline compiled.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// The device, with the limits of the adapter and the compressed texture formats
/// it has.
pub fn open_device(adapter: &wgpu::Adapter) -> anyhow::Result<(wgpu::Device, wgpu::Queue)> {
    let desc = wgpu::DeviceDescriptor {
        required_limits: adapter.limits(),
        required_features: adapter.features() & rendererx::OPTIONAL_FEATURES,
        ..Default::default()
    };
    pollster::block_on(adapter.request_device(&desc)).context("creating the GPU device")
}

/// RendererX for windows, in background-compile mode, and the thread that compiles
/// its pipelines: on a cold driver cache each takes 65–100 ms, which on the UI thread
/// froze the window on the first 3D file and stalled a frame per shading model. Now
/// a model shows as soon as clay is ready and switches to its own shading as each
/// pipeline arrives.
pub struct Rx {
    renderer: Arc<rendererx::Renderer>,
    /// Wakes the worker; one slot, so nudges while it is busy merge.
    kick: std::sync::mpsc::SyncSender<()>,
}

impl Rx {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, wake: Wake) -> Self {
        let desc = rendererx::RendererDesc { background_compile: true, ..Default::default() };
        let renderer = Arc::new(rendererx::Renderer::new(device, queue, desc));
        let (kick, nudged) = std::sync::mpsc::sync_channel::<()>(1);
        let worker = renderer.clone();
        // Exits when the shell (and so `kick`) goes away.
        let spawned = std::thread::Builder::new().name("shader compile".into()).spawn(move || {
            while nudged.recv().is_ok() {
                while worker.compile_one() {
                    wake();
                }
            }
        });
        if spawned.is_err() {
            // No thread: compile here instead, as foreground hosts do.
            renderer.compile_pending();
        }
        let rx = Self { renderer, kick };
        // The clay pipelines queued by `new`.
        rx.nudge();
        rx
    }

    fn nudge(&self) {
        let _ = self.kick.try_send(());
    }
}

/// Textures uploaded through the host, kept for reuse when freed: page tiles are all
/// one size, and zooming frees and asks for them by the thousand. A freed texture of
/// the size asked for is written over instead of a new one made, so zooming allocates
/// nothing, and nothing waits on wgpu's deferred destruction to be given back.
#[derive(Default)]
pub struct TexturePool {
    /// Every texture the pool made, by its id: the texture and its size.
    made: HashMap<TextureId, (wgpu::Texture, u32, u32)>,
    /// Freed ones, ready to be written over.
    spare: Vec<TextureId>,
    spare_bytes: usize,
}

/// Freed textures kept for reuse, at most: about a screenful of tiles.
const SPARE_BYTES: usize = if cfg!(any(target_os = "ios", target_os = "android")) { 32 << 20 } else { 64 << 20 };

impl TexturePool {
    fn take(&mut self, width: u32, height: u32) -> Option<(TextureId, &wgpu::Texture)> {
        let i = self.spare.iter().position(|id| self.made.get(id).is_some_and(|(_, w, h)| (*w, *h) == (width, height)))?;
        let id = self.spare.swap_remove(i);
        self.spare_bytes -= (width * height * 4) as usize;
        self.made.get(&id).map(|(t, _, _)| (id, t))
    }

    /// A texture of a page that closed: given up (its sizes won't be asked for again,
    /// and a spare of them would only hold memory). Returns true when the caller should
    /// unregister it.
    pub fn forget(&mut self, id: TextureId) -> bool {
        if let Some(i) = self.spare.iter().position(|s| *s == id) {
            self.spare.swap_remove(i);
            if let Some((_, w, h)) = self.made.get(&id) {
                self.spare_bytes -= (*w * *h * 4) as usize;
            }
        }
        self.made.remove(&id);
        true
    }

    /// The spares, given up: a page closed, and what it freed may not be asked for
    /// again. Returns them, for the caller to unregister.
    pub fn drop_spares(&mut self) -> Vec<TextureId> {
        self.spare_bytes = 0;
        let spare = std::mem::take(&mut self.spare);
        for id in &spare {
            self.made.remove(id);
        }
        spare
    }

    /// A texture a view freed while it goes on (an evicted tile): kept for reuse if
    /// there's room, else given up.
    /// Returns true when the caller should unregister it from the renderer.
    pub fn release(&mut self, id: TextureId) -> bool {
        let Some((_, w, h)) = self.made.get(&id) else { return true };
        let bytes = (*w * *h * 4) as usize;
        if self.spare_bytes + bytes <= SPARE_BYTES {
            self.spare.push(id);
            self.spare_bytes += bytes;
            false
        } else {
            self.made.remove(&id);
            true
        }
    }
}

/// libviewer's host for one frame on a wgpu device.
pub struct Gpu<'a> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub renderer: &'a mut libgui_wgpu::Renderer,
    /// RendererX on the same device, created the first time a 3D view needs it so
    /// startup never pays for its pipelines.
    pub rx: &'a mut Option<Rx>,
    pub scale: f32,
    /// Set by `uploads_done`: the shell waits for the GPU before the next frame.
    pub settle: &'a mut bool,
    /// Textures views freed this frame; released to the pool once the frame is prepared.
    pub freed: &'a mut Vec<TextureId>,
    /// Uploaded textures, reused when freed.
    pub pool: &'a mut TexturePool,
    /// See `Host::trackpad_pinch`.
    pub pinch: f32,
    pub wake: &'a Wake,
    /// An iPhone or iPad: hints speak of taps and fingers.
    pub touch_screen: bool,
}

impl Host for Gpu<'_> {
    fn image_opener(&self) -> Option<libviewer::ImageOpener> {
        Some(Arc::new(viewer_formats::open_image))
    }

    fn trackpad_pinch(&self) -> f32 {
        self.pinch
    }

    fn touch_screen(&self) -> bool {
        self.touch_screen
    }

    fn waker(&self) -> libviewer::Waker {
        let wake = self.wake.clone();
        Arc::new(move || wake())
    }

    fn max_side(&self) -> u32 {
        self.device.limits().max_texture_dimension_2d
    }

    fn upload(&mut self, width: u32, height: u32, rgba: &[u8]) -> TextureId {
        let size = wgpu::Extent3d { width, height, depth_or_array_layers: 1 };
        let layout = wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(width * 4), rows_per_image: Some(height) };
        if let Some((id, texture)) = self.pool.take(width, height) {
            self.queue.write_texture(texture.as_image_copy(), rgba, layout, size);
            return id;
        }
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("image tile"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(texture.as_image_copy(), rgba, layout, size);
        // The renderer's bind group keeps the view alive; the pool keeps the texture,
        // to write over when it's freed.
        let id = self.renderer.register_texture(&texture.create_view(&Default::default()));
        self.pool.made.insert(id, (texture, width, height));
        id
    }

    fn pixels_per_point(&self) -> f32 {
        self.scale
    }

    fn uploads_done(&mut self) {
        *self.settle = true;
    }

    fn free(&mut self, id: TextureId) {
        self.freed.push(id);
    }

    fn render_scene(&mut self, st: &mut SceneGpu, scene: &CpuScene, size: (u32, u32), camera: &Camera, background: [f32; 4]) -> Frame3d {
        let rx = self.rx.get_or_insert_with(|| Rx::new(self.device, self.queue, self.wake.clone()));
        let busy = draw_scene((&rx.renderer, self.device, self.queue), st, scene, size, camera, background, false);
        // The frame queued pipelines it lacked (drawing clay, or nothing, meanwhile).
        if rx.renderer.pipelines_pending() {
            rx.nudge();
        }
        // The target is recreated on resize; point the UI's texture at the new one.
        let view = st.view.as_ref().expect("created by draw_scene");
        if let Some(target) = view.target_view() {
            let generation = view.target_generation();
            match st.texture {
                Some((_, g)) if g == generation => {}
                Some((id, _)) => {
                    self.renderer.update_texture(id, target);
                    st.texture = Some((id, generation));
                }
                None => st.texture = Some((self.renderer.register_texture(target), generation)),
            }
        }
        Frame3d::shown(st.texture(), busy)
    }
}
